// Verifies a CometBFT signed successor for an authenticated native Fern AppHash.
package main

import (
	"bytes"
	"crypto/sha256"
	"encoding/hex"
	"encoding/json"
	"errors"
	"flag"
	"fmt"
	"io"
	"math"
	"os"
	"strings"

	"github.com/cometbft/cometbft/crypto/ed25519"
	cmtjson "github.com/cometbft/cometbft/libs/json"
	core "github.com/cometbft/cometbft/rpc/core/types"
	"github.com/cometbft/cometbft/types"
)

const maxProofInput = 32 << 20

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

type committeeHandoff struct {
	Commit  core.ResultCommit  `json:"commit"`
	Members []*types.Validator `json:"members"`
}

func committeeAt(genesis *types.GenesisDoc, height int64, changes []committeeHandoff) (*types.ValidatorSet, error) {
	current, err := genesisCommittee(genesis)
	if err != nil {
		return nil, err
	}
	active := genesis.InitialHeight
	if len(changes) > 256 {
		return nil, errors.New("too many committee handoffs")
	}
	for _, change := range changes {
		signed := change.Commit.SignedHeader
		if signed.Header == nil || signed.Height < active || signed.Height >= height {
			return nil, errors.New("unordered or premature committee handoff")
		}
		if err := verifyCommit(genesis.ChainID, signed, current); err != nil {
			return nil, err
		}
		successor, err := validatorSet(change.Members)
		if err != nil {
			return nil, err
		}
		if !bytes.Equal(signed.NextValidatorsHash, successor.Hash()) {
			return nil, errors.New("handoff does not authenticate the successor committee")
		}
		active = signed.Height + 1
		current = successor
	}
	return current, nil
}

func run() error {
	genesis := flag.String("genesis", "", "trusted CometBFT genesis file")
	commit := flag.String("commit", "", "signed CometBFT successor from the commit endpoint")
	handoffs := flag.String("handoffs", "", "optional signed committee handoff history")
	height := flag.Int64("height", 0, "native Fern application height")
	metadata := flag.Bool("metadata", false, "output authenticated checkpoint metadata as JSON")
	fernGenesis := flag.String("fern-genesis", "", "expected Fern genesis Header ID")
	appHashArg := flag.String("app-hash", "", "expected native Fern AppHash")
	flag.Parse()
	appHash, err := hex.DecodeString(strings.TrimPrefix(*appHashArg, "0x"))
	if err != nil || len(appHash) != 32 || *height < 1 || *height == math.MaxInt64 ||
		*genesis == "" || *commit == "" || flag.NArg() != 0 {
		return errors.New("provide -genesis, -commit, -height and a 32-byte -app-hash")
	}
	trusted, err := types.GenesisDocFromFile(*genesis)
	if err != nil {
		return err
	}
	if *fernGenesis != "" {
		var state struct {
			FernGenesis string `json:"fern_genesis"`
		}
		if err := json.Unmarshal(trusted.AppState, &state); err != nil {
			return err
		}
		if state.FernGenesis != *fernGenesis {
			return errors.New("CometBFT app state does not match the expected Fern genesis")
		}
	}
	raw, err := readLimited(*commit)
	if err != nil {
		return err
	}
	var signed core.ResultCommit
	if err := cmtjson.Unmarshal(raw, &signed); err != nil {
		return err
	}
	var changes []committeeHandoff
	handoffHash := sha256.Sum256(nil)
	if *handoffs != "" {
		raw, err := readLimited(*handoffs)
		if err != nil {
			return err
		}
		handoffHash = sha256.Sum256(raw)
		if err := cmtjson.Unmarshal(raw, &changes); err != nil {
			return err
		}
	}
	voters, err := committeeAt(trusted, *height+1, changes)
	if err != nil {
		return err
	}
	if err := verifyCommit(trusted.ChainID, signed.SignedHeader, voters); err != nil {
		return err
	}
	if signed.Height != *height+1 || !bytes.Equal(signed.AppHash, appHash) {
		return errors.New("commit does not authenticate this Fern height and AppHash")
	}
	if *metadata {
		block := signed.LastBlockID
		if err := block.ValidateBasic(); err != nil || !block.IsComplete() {
			return errors.New("signed successor lacks the prior complete Block ID")
		}
		return json.NewEncoder(os.Stdout).Encode(struct {
			Block         types.BlockID `json:"block"`
			Successor     types.BlockID `json:"successor"`
			Committee     string        `json:"committee"`
			NextCommittee string        `json:"next_committee"`
			Handoffs      string        `json:"handoffs"`
		}{
			Block: block, Successor: signed.Commit.BlockID,
			Committee:     hex.EncodeToString(signed.ValidatorsHash),
			NextCommittee: hex.EncodeToString(signed.NextValidatorsHash),
			Handoffs:      hex.EncodeToString(handoffHash[:]),
		})
	}
	fmt.Printf("verified height=%d successor=%d app_hash=%x\n", *height, signed.Height, appHash)
	return nil
}

func main() {
	if err := run(); err != nil {
		fmt.Fprintln(os.Stderr, err)
		os.Exit(1)
	}
}
