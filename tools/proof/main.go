// Exports and independently verifies CometBFT-backed CKB proofs.
package main

import (
	"bytes"
	"encoding/binary"
	"encoding/json"
	"errors"
	"flag"
	"fmt"
	"io"
	"os"

	"github.com/cometbft/cometbft/crypto/ed25519"
	cmtjson "github.com/cometbft/cometbft/libs/json"
	core "github.com/cometbft/cometbft/rpc/core/types"
	"github.com/cometbft/cometbft/types"
)

const ckbHashSize = 32
const withdrawalReceiptSize = ckbHashSize + 8
const maxProofInput = 32 << 20

type executionProof struct {
	Commit     core.ResultCommit       `json:"commit"`
	NextCommit core.ResultCommit       `json:"next_commit"`
	Tx         core.ResultTx           `json:"tx"`
	Results    core.ResultBlockResults `json:"results"`
	Handoffs   []committeeHandoff      `json:"handoffs,omitempty"`
}

type committeeHandoff struct {
	Commit  core.ResultCommit  `json:"commit"`
	Members []*types.Validator `json:"members"`
}

func validatorSet(members []*types.Validator) (*types.ValidatorSet, error) {
	if len(members) == 0 || len(members) > 256 {
		return nil, errors.New("invalid validator committee size")
	}
	var power int64
	addresses := make(map[string]struct{}, len(members))
	for _, member := range members {
		if member == nil {
			return nil, errors.New("missing validator")
		}
		key, ok := member.PubKey.(ed25519.PubKey)
		if !ok || len(key) != ed25519.PubKeySize {
			return nil, errors.New("invalid Ed25519 validator key")
		}
		if err := member.ValidateBasic(); err != nil {
			return nil, err
		}
		if member.VotingPower <= 0 || member.VotingPower > types.MaxTotalVotingPower-power {
			return nil, errors.New("invalid committee power")
		}
		power += member.VotingPower
		address := string(member.Address)
		if _, ok := addresses[address]; ok {
			return nil, errors.New("duplicate committee member")
		}
		addresses[address] = struct{}{}
	}
	return types.NewValidatorSet(members), nil
}

func verifyCommit(chain string, signed types.SignedHeader, committee *types.ValidatorSet) error {
	if err := signed.ValidateBasic(chain); err != nil {
		return err
	}
	if !bytes.Equal(signed.ValidatorsHash, committee.Hash()) {
		return errors.New("header validator set differs from the authenticated committee")
	}
	if err := committee.VerifyCommit(chain, signed.Commit.BlockID, signed.Height, signed.Commit); err != nil {
		return fmt.Errorf("verify commit at %d: %w", signed.Height, err)
	}
	return nil
}

func genesisCommittee(genesis *types.GenesisDoc) (*types.ValidatorSet, error) {
	if len(genesis.Validators) == 0 {
		return nil, errors.New("trusted genesis has no validators")
	}
	members := make([]*types.Validator, 0, len(genesis.Validators))
	for _, member := range genesis.Validators {
		members = append(members, types.NewValidator(member.PubKey, member.Power))
	}
	return validatorSet(members)
}

func verifySuccessor(genesis *types.GenesisDoc, first, next types.SignedHeader, handoffs []committeeHandoff) error {
	committee, err := genesisCommittee(genesis)
	if err != nil {
		return err
	}
	if first.Header == nil || next.Header == nil || len(handoffs) > 256 {
		return errors.New("missing headers or too many committee handoffs")
	}
	firstCommittee := committee
	priorHeight := int64(0)
	for _, handoff := range handoffs {
		signed := handoff.Commit.SignedHeader
		if signed.Header == nil || signed.Height <= priorHeight || signed.Height > first.Height {
			return errors.New("committee handoffs must be strictly ordered before the receipt successor")
		}
		if err := verifyCommit(genesis.ChainID, signed, committee); err != nil {
			return err
		}
		incoming, err := validatorSet(handoff.Members)
		if err != nil {
			return err
		}
		if !bytes.Equal(signed.NextValidatorsHash, incoming.Hash()) || bytes.Equal(committee.Hash(), incoming.Hash()) {
			return errors.New("handoff did not authenticate a new committee")
		}
		if signed.Height < first.Height {
			firstCommittee = incoming
		} else if !bytes.Equal(signed.Hash(), first.Hash()) {
			return errors.New("handoff and transaction certificates disagree at the same height")
		}
		priorHeight, committee = signed.Height, incoming
	}
	if err := verifyCommit(genesis.ChainID, first, firstCommittee); err != nil {
		return err
	}
	if err := verifyCommit(genesis.ChainID, next, committee); err != nil {
		return err
	}
	if next.Height != first.Height+1 || !next.LastBlockID.Equals(first.Commit.BlockID) ||
		!bytes.Equal(first.NextValidatorsHash, next.ValidatorsHash) {
		return errors.New("execution receipt must be certified by the next block")
	}
	return nil
}

func verify(genesis *types.GenesisDoc, proof *executionProof) error {
	first, next := proof.Commit.SignedHeader, proof.NextCommit.SignedHeader
	if err := verifySuccessor(genesis, first, next, proof.Handoffs); err != nil {
		return err
	}

	tx := proof.Tx
	if tx.Height != first.Height || int64(tx.Index) != tx.Proof.Proof.Index ||
		!bytes.Equal(tx.Hash, tx.Tx.Hash()) || !bytes.Equal(tx.Tx, tx.Proof.Data) {
		return errors.New("transaction identity or position mismatch")
	}
	if err := tx.Proof.Validate(first.DataHash); err != nil {
		return err
	}
	results := proof.Results
	if results.Height != tx.Height || int64(len(results.TxsResults)) != tx.Proof.Proof.Total ||
		uint64(tx.Index) >= uint64(len(results.TxsResults)) {
		return errors.New("receipt set differs from the proved transaction list")
	}
	for _, result := range results.TxsResults {
		if result == nil {
			return errors.New("missing ABCI result")
		}
	}
	if !bytes.Equal(next.LastResultsHash, types.NewResults(results.TxsResults).Hash()) ||
		!bytes.Equal(next.AppHash, results.AppHash) {
		return errors.New("execution results or application hash differ from the signed header")
	}
	result := results.TxsResults[tx.Index]
	if result.Code != 0 || result.GasUsed < 0 ||
		(len(result.Data) != ckbHashSize && len(result.Data) != withdrawalReceiptSize) {
		return errors.New("receipt does not show a successful CKB transaction")
	}
	// The receipt from the results tree is authoritative. Also reject a bundle
	// that disagrees with the copy attached to the transaction RPC response.
	if result.Code != tx.TxResult.Code || result.GasUsed != tx.TxResult.GasUsed ||
		result.GasWanted != tx.TxResult.GasWanted || !bytes.Equal(result.Data, tx.TxResult.Data) {
		return errors.New("transaction and block execution receipts disagree")
	}
	return nil
}

func readLimited(path string) ([]byte, error) {
	file, err := os.Open(path)
	if err != nil {
		return nil, err
	}
	defer file.Close()
	data, err := io.ReadAll(io.LimitReader(file, maxProofInput+1))
	if err != nil {
		return nil, err
	}
	if len(data) > maxProofInput {
		return nil, errors.New("proof input exceeds size limit")
	}
	return data, nil
}

func readProof(path string) (*executionProof, error) {
	data, err := readLimited(path)
	if err != nil {
		return nil, err
	}
	var proof executionProof
	if err := cmtjson.Unmarshal(data, &proof); err != nil {
		return nil, err
	}
	return &proof, nil
}

func run() error {
	genesisPath := flag.String("genesis", "", "trusted CometBFT genesis file")
	proofPath := flag.String("proof", "", "execution proof bundle")
	cellPath := flag.String("cell-proof", "", "Live Cell checkpoint proof bundle")
	rpc := flag.String("rpc", "", "CometBFT RPC URL to export a verified execution proof from retained blocks")
	height := flag.Int64("height", 0, "expected checkpoint, export transaction or upgrade certificate height")
	txHash := flag.String("ckb-tx", "", "expected CKB transaction hash for proof export, hex-encoded")
	outpoint := flag.String("out-point", "", "expected packed Molecule OutPoint, hex-encoded")
	handoffs := flag.String("handoffs", "", "optional JSON committee handoffs for proof export or Live Cell verification")
	jsonOutput := flag.Bool("json", false, "print authenticated transaction bytes and receipt as JSON")
	offline := flag.String("offline", "", "comma-separated validator addresses to stop after verifying the receipt successor")
	flag.Parse()
	modes := 0
	for _, value := range []string{*proofPath, *cellPath, *rpc} {
		if value != "" {
			modes++
		}
	}
	if *genesisPath == "" || modes != 1 || flag.NArg() != 0 {
		return errors.New("provide trusted -genesis and one of -proof, -cell-proof or -rpc")
	}
	if *rpc == "" && *txHash != "" {
		return errors.New("-ckb-tx requires -rpc")
	}
	if *offline != "" && (*proofPath == "" || *height <= 0 || *jsonOutput) {
		return errors.New("-offline requires -proof and an expected successor -height")
	}
	genesis, err := types.GenesisDocFromFile(*genesisPath)
	if err != nil {
		return err
	}
	if *cellPath != "" {
		return verifyCellFile(genesis, *cellPath, *handoffs, *height, *outpoint)
	}
	if *rpc != "" {
		if *outpoint != "" || *jsonOutput {
			return errors.New("-rpc exports an execution proof bundle and does not take -out-point or -json")
		}
		return exportExecutionProof(genesis, *rpc, *height, *txHash, *handoffs, os.Stdout)
	}
	if (*height != 0 && *offline == "") || *outpoint != "" || *handoffs != "" {
		return errors.New("-height, -out-point or -handoffs is incompatible with this proof mode")
	}
	proof, err := readProof(*proofPath)
	if err != nil {
		return err
	}
	if err := verify(genesis, proof); err != nil {
		return err
	}
	if *offline != "" {
		quorum, err := upgradeQuorum(genesis, proof, *height, *offline)
		if err != nil {
			return err
		}
		return json.NewEncoder(os.Stdout).Encode(quorum)
	}
	result := proof.Results.TxsResults[proof.Tx.Index]
	ckbTxHash := result.Data[:ckbHashSize]
	if *jsonOutput {
		var withdrawalStart *uint64
		if len(result.Data) == withdrawalReceiptSize {
			nonce := binary.LittleEndian.Uint64(result.Data[ckbHashSize:])
			withdrawalStart = &nonce
		}
		return json.NewEncoder(os.Stdout).Encode(struct {
			ChainID         string  `json:"chain_id"`
			Height          int64   `json:"height"`
			CKBTxHash       string  `json:"ckb_tx_hash"`
			AppHash         string  `json:"app_hash"`
			Transaction     string  `json:"transaction"`
			WithdrawalStart *uint64 `json:"withdrawal_start,omitempty"`
		}{
			ChainID: genesis.ChainID, Height: proof.Tx.Height,
			CKBTxHash:       fmt.Sprintf("0x%x", ckbTxHash),
			AppHash:         fmt.Sprintf("0x%x", proof.Results.AppHash),
			Transaction:     fmt.Sprintf("0x%x", []byte(proof.Tx.Tx)),
			WithdrawalStart: withdrawalStart,
		})
	}
	fmt.Printf("height=%d ckb_tx=0x%x cycles=%d app_hash=0x%x\n",
		proof.Tx.Height, ckbTxHash, result.GasUsed, proof.Results.AppHash)
	return nil
}

func main() {
	if err := run(); err != nil {
		fmt.Fprintln(os.Stderr, err)
		os.Exit(1)
	}
}
