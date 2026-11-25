package main

import (
	"bytes"
	"encoding/binary"
	"encoding/hex"
	"encoding/json"
	"errors"
	"fmt"
	"os"
	"strings"

	"github.com/cometbft/cometbft/crypto/ed25519"
	cmtjson "github.com/cometbft/cometbft/libs/json"
	core "github.com/cometbft/cometbft/rpc/core/types"
	"github.com/cometbft/cometbft/types"
)

type directStep struct {
	Commit       core.ResultCommit  `json:"commit"`
	Validators   []*types.Validator `json:"validators"`
	NativeHeader string             `json:"native_header"`
}

type directBundle struct {
	Proof            string `json:"proof"`
	Tip              string `json:"tip"`
	InitialCommittee string `json:"initial_committee"`
	NativeHeight     int64  `json:"native_height"`
	CommitSizes      []int  `json:"commit_sizes"`
}

func buildDirectBundle(genesis *types.GenesisDoc, entries []directStep) (directBundle, error) {
	const maxSteps = 8
	const maxValidators = 128
	if len(entries) == 0 || len(entries) > maxSteps || genesis.InitialHeight != 1 {
		return directBundle{}, errors.New("direct proof requires 1–8 consecutive commits on a height-one chain")
	}
	initial, err := genesisCommittee(genesis)
	if err != nil {
		return directBundle{}, err
	}
	out := directBundle{InitialCommittee: hex.EncodeToString(initial.Hash())}
	proof := []byte{byte(len(entries))}
	var oldBlock types.BlockID
	var previousSet, nextSet []byte
	var priorHeight int64
	for i, entry := range entries {
		if len(entry.Validators) > maxValidators {
			return directBundle{}, errors.New("direct proof committee exceeds 128 validators")
		}
		set, err := validatorSet(entry.Validators)
		if err != nil {
			return directBundle{}, err
		}
		signed := entry.Commit.SignedHeader
		if err := verifyCommit(genesis.ChainID, signed, set); err != nil {
			return directBundle{}, err
		}
		if i == 0 && signed.Height == 1 && !bytes.Equal(initial.Hash(), set.Hash()) {
			return directBundle{}, errors.New("first commit does not use the genesis committee")
		}
		if i > 0 {
			if signed.Height != priorHeight+1 || !signed.LastBlockID.Equals(oldBlock) {
				return directBundle{}, errors.New("direct proof jumps height or changes its previous block")
			}
			if !bytes.Equal(nextSet, set.Hash()) {
				return directBundle{}, errors.New("direct proof breaks the signed committee handoff")
			}
		}
		header := signed.Header
		encoded, err := header.ToProto().Marshal()
		if err != nil || len(encoded) == 0 || len(encoded) > 1024 {
			return directBundle{}, errors.New("invalid direct proof header")
		}
		native, err := hex.DecodeString(strings.TrimPrefix(entry.NativeHeader, "0x"))
		if err != nil || len(native) != 208 || binary.LittleEndian.Uint64(native[:8]) != uint64(signed.Height-1) {
			return directBundle{}, errors.New("invalid native header height or size")
		}
		block := signed.Commit.BlockID
		if !block.IsComplete() || len(header.AppHash) != 32 || len(header.NextValidatorsHash) != 32 {
			return directBundle{}, errors.New("invalid direct proof commitments")
		}
		start := len(proof)
		proof = binary.LittleEndian.AppendUint16(proof, uint16(len(encoded)))
		proof = append(proof, encoded...)
		proof = binary.LittleEndian.AppendUint32(proof, block.PartSetHeader.Total)
		proof = append(proof, block.PartSetHeader.Hash...)
		proof = binary.LittleEndian.AppendUint32(proof, uint32(signed.Commit.Round))
		proof = append(proof, native...)
		if bytes.Equal(previousSet, set.Hash()) {
			proof = binary.LittleEndian.AppendUint16(proof, 0)
		} else {
			proof = binary.LittleEndian.AppendUint16(proof, uint16(set.Size()))
			for _, member := range set.Validators {
				proof = append(proof, member.PubKey.(ed25519.PubKey)...)
				proof = binary.LittleEndian.AppendUint64(proof, uint64(member.VotingPower))
			}
		}
		voteCount := 0
		position := len(proof)
		proof = binary.LittleEndian.AppendUint16(proof, 0)
		for index, sig := range signed.Commit.Signatures {
			if sig.BlockIDFlag != types.BlockIDFlagCommit {
				continue
			}
			proof = binary.LittleEndian.AppendUint16(proof, uint16(index))
			proof = binary.LittleEndian.AppendUint64(proof, uint64(sig.Timestamp.Unix()))
			proof = binary.LittleEndian.AppendUint32(proof, uint32(sig.Timestamp.Nanosecond()))
			proof = append(proof, sig.Signature...)
			voteCount++
		}
		if voteCount == 0 {
			return directBundle{}, fmt.Errorf("commit at %d has no block votes", signed.Height)
		}
		binary.LittleEndian.PutUint16(proof[position:], uint16(voteCount))
		out.CommitSizes = append(out.CommitSizes, len(proof)-start)

		tip := append([]byte{}, block.Hash...)
		tip = binary.LittleEndian.AppendUint32(tip, block.PartSetHeader.Total)
		tip = append(tip, block.PartSetHeader.Hash...)
		tip = append(tip, header.NextValidatorsHash...)
		tip = append(tip, native...)
		out.Tip = "0x" + hex.EncodeToString(tip)
		out.NativeHeight = signed.Height - 1
		oldBlock, previousSet, nextSet, priorHeight = block, set.Hash(), header.NextValidatorsHash, signed.Height
	}
	out.Proof = "0x" + hex.EncodeToString(proof)
	return out, nil
}

func exportDirect(genesisPath, input string) error {
	trusted, err := types.GenesisDocFromFile(genesisPath)
	if err != nil {
		return err
	}
	raw, err := readLimited(input)
	if err != nil {
		return err
	}
	var entries []directStep
	if err := cmtjson.Unmarshal(raw, &entries); err != nil {
		return err
	}
	bundle, err := buildDirectBundle(trusted, entries)
	if err != nil {
		return err
	}
	return json.NewEncoder(os.Stdout).Encode(bundle)
}
