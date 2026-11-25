package main

import (
	"bytes"
	"encoding/hex"
	"encoding/json"
	"fmt"
	"os"
	"path/filepath"
	"testing"
	"time"

	"github.com/cometbft/cometbft/crypto/ed25519"
	version "github.com/cometbft/cometbft/proto/tendermint/version"
	core "github.com/cometbft/cometbft/rpc/core/types"
	"github.com/cometbft/cometbft/types"
)

func TestDirectCertificateCostFixtures(t *testing.T) {
	path := os.Getenv("STEM_DIRECT_COST_DIR")
	// Stem AppHash of the fixed 208-byte synthetic genesis header on network 05…05.
	app, err := hex.DecodeString("8cf9a41fa2f82add6785badb7edbdc17a6cc0182b314612a629d60751357779a")
	if err != nil {
		t.Fatal(err)
	}
	for _, count := range []int{4, 16, 32, 64, 100, 128} {
		chain := "stem-direct-cost"
		genesis := &types.GenesisDoc{ChainID: chain, InitialHeight: 1}
		members := make([]*types.Validator, 0, count)
		keys := make(map[string]ed25519.PrivKey, count)
		for index := 0; index < count; index++ {
			key := ed25519.GenPrivKeyFromSecret([]byte(fmt.Sprintf("direct cost test member %d", index)))
			power := int64(1 + index%3)
			validator := types.NewValidator(key.PubKey(), power)
			keys[string(validator.Address)] = key
			members = append(members, validator)
			genesis.Validators = append(genesis.Validators, types.GenesisValidator{
				Address: validator.Address, PubKey: validator.PubKey, Power: power,
			})
		}
		set := types.NewValidatorSet(members)
		header := &types.Header{
			Version: version.Consensus{Block: 11, App: 1},
			ChainID: chain, Height: 1, Time: time.Unix(175_000, 123).UTC(),
			ValidatorsHash: set.Hash(), NextValidatorsHash: set.Hash(),
			ProposerAddress: set.Validators[0].Address, AppHash: app,
		}
		for _, thresholdOnly := range []bool{true, false} {
			commit := &types.Commit{
				Height: 1, Round: 1, BlockID: types.BlockID{
					Hash:          header.Hash(),
					PartSetHeader: types.PartSetHeader{Total: 1, Hash: bytes.Repeat([]byte{11}, 32)},
				},
			}
			var power int64
			signed := 0
			for i, member := range set.Validators {
				if thresholdOnly && power*3 > set.TotalVotingPower()*2 {
					commit.Signatures = append(commit.Signatures, types.NewCommitSigAbsent())
					continue
				}
				commit.Signatures = append(commit.Signatures, types.CommitSig{
					BlockIDFlag: types.BlockIDFlagCommit, ValidatorAddress: member.Address,
					Timestamp: time.Unix(175_001, int64(i)).UTC(),
				})
				sig, err := keys[string(member.Address)].Sign(commit.VoteSignBytes(chain, int32(i)))
				if err != nil {
					t.Fatal(err)
				}
				commit.Signatures[i].Signature = sig
				power += member.VotingPower
				signed++
			}
			entry := directStep{
				Commit:     core.ResultCommit{SignedHeader: types.SignedHeader{Header: header, Commit: commit}},
				Validators: set.Validators, NativeHeader: hex.EncodeToString(make([]byte, 208)),
			}
			bundle, err := buildDirectBundle(genesis, []directStep{entry})
			if err != nil {
				t.Fatal(err)
			}
			if path != "" {
				profile := "all"
				if thresholdOnly {
					profile = "quorum"
				}
				data, err := json.Marshal(struct {
					Committee      int          `json:"committee"`
					Signatures     int          `json:"signatures"`
					Profile        string       `json:"profile"`
					GenesisAppHash string       `json:"genesis_app_hash"`
					Bundle         directBundle `json:"bundle"`
				}{count, signed, profile, hex.EncodeToString(app), bundle})
				if err != nil {
					t.Fatal(err)
				}
				if err := os.WriteFile(filepath.Join(path, fmt.Sprintf("%03d-%s.json", count, profile)), data, 0o644); err != nil {
					t.Fatal(err)
				}
			}
		}
	}
}
