package main

import (
	"bytes"
	"encoding/hex"
	"encoding/json"
	"os"
	"path/filepath"
	"strconv"
	"strings"
	"testing"

	cmtjson "github.com/cometbft/cometbft/libs/json"
	core "github.com/cometbft/cometbft/rpc/core/types"
	"github.com/cometbft/cometbft/types"
)

// Verifies exported commits from tests/native_store/comet.rs against its
// generated genesis committee and the native AppHash computed in Rust.
func TestNativeCommitSignatures(t *testing.T) {
	root := os.Getenv("STEM_NATIVE_EVIDENCE")
	if root == "" {
		t.Skip("set STEM_NATIVE_EVIDENCE to the four-node test output")
	}
	genesis, err := types.GenesisDocFromFile(filepath.Join(root, "net/node0/config/genesis.json"))
	if err != nil {
		t.Fatal(err)
	}
	voters, err := genesisCommittee(genesis)
	if err != nil {
		t.Fatal(err)
	}
	appHashText, err := os.ReadFile(filepath.Join(root, "app-hash"))
	if err != nil {
		t.Fatal(err)
	}
	appHash, err := hex.DecodeString(strings.TrimSpace(string(appHashText)))
	if err != nil || len(appHash) != 32 {
		t.Fatal("invalid native AppHash")
	}
	raw, err := os.ReadFile(filepath.Join(root, "header.bin"))
	if err != nil || len(raw) != 208 {
		t.Fatal("invalid fixed-length native Header")
	}
	height := int64(0)
	for i := 7; i >= 0; i-- {
		height = height<<8 | int64(raw[i])
	}
	for index := 0; index < 4; index++ {
		entry, err := readLimited(filepath.Join(root, "commit-"+strconv.Itoa(index)+".json"))
		if err != nil {
			t.Fatal(err)
		}
		var proof core.ResultCommit
		if err := cmtjson.Unmarshal(entry, &proof); err != nil {
			t.Fatal(err)
		}
		if err := verifyCommit(genesis.ChainID, proof.SignedHeader, voters); err != nil {
			t.Fatal(err)
		}
		if proof.Height != height+1 || !bytes.Equal(proof.AppHash, appHash) {
			t.Fatalf("replica %d does not authenticate native height %d", index, height)
		}
	}
	t.Logf("verified four replicas' signed successor for native height %d", height)
}

// Verifies both dynamic Head proof bundles against the generated committee.
// The Rust test authenticates the Patricia paths against the declared AppHash.
func TestLineageCommandSignatures(t *testing.T) {
	root := os.Getenv("STEM_NATIVE_EVIDENCE")
	if root == "" {
		t.Skip("set STEM_NATIVE_EVIDENCE to the four-node test output")
	}
	genesis, err := types.GenesisDocFromFile(filepath.Join(root, "net/node0/config/genesis.json"))
	if err != nil {
		t.Fatal(err)
	}
	voters, err := genesisCommittee(genesis)
	if err != nil {
		t.Fatal(err)
	}
	raw, err := readLimited(filepath.Join(root, "lineage-commands.json"))
	if err != nil {
		t.Fatal(err)
	}
	var evidence struct {
		Proofs [][]struct {
			Height  int64           `json:"height"`
			AppHash string          `json:"app_hash"`
			Commit  json.RawMessage `json:"signed_successor"`
		} `json:"proofs"`
	}
	if err := json.Unmarshal(raw, &evidence); err != nil || len(evidence.Proofs) != 2 {
		t.Fatalf("invalid dynamic Head evidence: %v", err)
	}
	for _, set := range evidence.Proofs {
		if len(set) != 5 {
			t.Fatal("missing proof replica")
		}
		for _, entry := range set {
			expected, err := hex.DecodeString(entry.AppHash)
			if err != nil || len(expected) != 32 {
				t.Fatal("invalid dynamic Head AppHash")
			}
			var proof core.ResultCommit
			if err := cmtjson.Unmarshal(entry.Commit, &proof); err != nil {
				t.Fatal(err)
			}
			if err := verifyCommit(genesis.ChainID, proof.SignedHeader, voters); err != nil {
				t.Fatal(err)
			}
			if proof.Height != entry.Height+1 || !bytes.Equal(proof.AppHash, expected) {
				t.Fatalf("commit does not authenticate the Head proof at height %d", entry.Height)
			}
		}
	}
	t.Log("verified ten signed successors for dynamic Head proof replicas")
}

// Verifies the certificate for a Stem block whose only executions were due work.
func TestKeeperWakeSignatures(t *testing.T) {
	root := os.Getenv("STEM_NATIVE_EVIDENCE")
	if root == "" {
		t.Skip("set STEM_NATIVE_EVIDENCE to the four-node test output")
	}
	genesis, err := types.GenesisDocFromFile(filepath.Join(root, "net/node0/config/genesis.json"))
	if err != nil {
		t.Fatal(err)
	}
	voters, err := genesisCommittee(genesis)
	if err != nil {
		t.Fatal(err)
	}
	raw, err := readLimited(filepath.Join(root, "keeper-wake.json"))
	if err != nil {
		t.Fatal(err)
	}
	var evidence struct {
		Due           int64           `json:"due"`
		AppHash       string          `json:"app_hash"`
		PendingHeight int64           `json:"pending_height"`
		PendingHash   string          `json:"pending_app_hash"`
		PendingCommit json.RawMessage `json:"pending_successor"`
		Wakes         []struct {
			Commit json.RawMessage `json:"signed_successor"`
		} `json:"wakes"`
	}
	if err := json.Unmarshal(raw, &evidence); err != nil || evidence.Due <= 0 || len(evidence.Wakes) != 5 {
		t.Fatalf("invalid Keeper evidence: %v", err)
	}
	expected, err := hex.DecodeString(evidence.AppHash)
	if err != nil || len(expected) != 32 {
		t.Fatal("invalid Keeper AppHash")
	}
	if len(evidence.PendingCommit) > 0 {
		expectedPending, err := hex.DecodeString(evidence.PendingHash)
		if err != nil || len(expectedPending) != 32 || evidence.PendingHeight >= evidence.Due {
			t.Fatal("invalid pending Keeper hash or height")
		}
		var proof core.ResultCommit
		if err := cmtjson.Unmarshal(evidence.PendingCommit, &proof); err != nil {
			t.Fatal(err)
		}
		if err := verifyCommit(genesis.ChainID, proof.SignedHeader, voters); err != nil {
			t.Fatal(err)
		}
		if proof.Height != evidence.PendingHeight+1 || !bytes.Equal(proof.AppHash, expectedPending) {
			t.Fatal("commit does not authenticate the pending Keeper queue")
		}
		t.Logf("verified signed pending Keeper queue at height %d", evidence.PendingHeight)
	}
	for _, entry := range evidence.Wakes {
		var proof core.ResultCommit
		if err := cmtjson.Unmarshal(entry.Commit, &proof); err != nil {
			t.Fatal(err)
		}
		if err := verifyCommit(genesis.ChainID, proof.SignedHeader, voters); err != nil {
			t.Fatal(err)
		}
		if proof.Height != evidence.Due+1 || !bytes.Equal(proof.AppHash, expected) {
			t.Fatalf("commit does not authenticate the Keeper execution at height %d", evidence.Due)
		}
	}
	t.Logf("verified five signed successor responses for the Keeper wake at height %d", evidence.Due)
}
