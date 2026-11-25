package main

import (
	"bytes"
	"encoding/hex"
	"errors"
	"fmt"
	"strings"

	"github.com/cometbft/cometbft/crypto"
	"github.com/cometbft/cometbft/types"
)

type upgradeVotingPower struct {
	Height                int64    `json:"height"`
	BlockHash             string   `json:"block_hash"`
	Offline               []string `json:"offline"`
	CommitteePower        int64    `json:"committee_power"`
	SignedPower           int64    `json:"signed_power"`
	RemainingSigningPower int64    `json:"remaining_signing_power"`
	RequiredSigningPower  int64    `json:"required_signing_power"`
}

// The execution proof and its committee handoffs are verified before this check.
func upgradeQuorum(genesis *types.GenesisDoc, proof *executionProof, height int64, offline string) (upgradeVotingPower, error) {
	next := proof.NextCommit.SignedHeader
	if next.Height != height {
		return upgradeVotingPower{}, fmt.Errorf("expected successor height %d, got %d", height, next.Height)
	}
	if !bytes.Equal(next.ValidatorsHash, next.NextValidatorsHash) {
		return upgradeVotingPower{}, errors.New("the certificate announces a committee change at the next height")
	}
	var committee *types.ValidatorSet
	var err error
	if len(proof.Handoffs) == 0 {
		committee, err = genesisCommittee(genesis)
	} else {
		committee, err = validatorSet(proof.Handoffs[len(proof.Handoffs)-1].Members)
	}
	if err != nil {
		return upgradeVotingPower{}, err
	}
	if !bytes.Equal(next.ValidatorsHash, committee.Hash()) {
		return upgradeVotingPower{}, errors.New("successor committee differs from the authenticated committee")
	}
	report := upgradeVotingPower{Height: height, BlockHash: fmt.Sprintf("%X", next.Hash()),
		CommitteePower: committee.TotalVotingPower()}
	report.RequiredSigningPower = 2*report.CommitteePower/3 + 1
	stopped := make(map[string]bool)
	for _, value := range strings.Split(offline, ",") {
		addr, err := hex.DecodeString(strings.TrimSpace(value))
		if err != nil || len(addr) != crypto.AddressSize {
			return upgradeVotingPower{}, fmt.Errorf("invalid validator address %q", value)
		}
		if _, validator := committee.GetByAddress(addr); validator == nil {
			return upgradeVotingPower{}, fmt.Errorf("validator %X is outside the certificate committee", addr)
		}
		key := string(addr)
		if stopped[key] {
			return upgradeVotingPower{}, fmt.Errorf("validator %X appears twice in -offline", addr)
		}
		stopped[key] = true
		report.Offline = append(report.Offline, fmt.Sprintf("%X", addr))
	}
	for index, sig := range next.Commit.Signatures {
		if sig.BlockIDFlag != types.BlockIDFlagCommit {
			continue
		}
		validator := committee.Validators[index]
		report.SignedPower += validator.VotingPower
		if !stopped[string(validator.Address)] {
			report.RemainingSigningPower += validator.VotingPower
		}
	}
	if report.RemainingSigningPower < report.RequiredSigningPower {
		return upgradeVotingPower{}, fmt.Errorf("certificate leaves voting power %d of %d after removing the requested validators, need at least %d",
			report.RemainingSigningPower, report.CommitteePower, report.RequiredSigningPower)
	}
	return report, nil
}
