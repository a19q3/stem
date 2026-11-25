package main

import (
	"bytes"
	"encoding/hex"
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
	root := os.Getenv("FERN_NATIVE_EVIDENCE")
	if root == "" {
		t.Skip("set FERN_NATIVE_EVIDENCE to the four-node test output")
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
