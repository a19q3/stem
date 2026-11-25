package main

import (
	"bytes"
	"context"
	"crypto/sha256"
	"encoding/binary"
	"testing"
	"time"

	abci "github.com/cometbft/cometbft/abci/types"
	"github.com/cometbft/cometbft/config"
	"github.com/cometbft/cometbft/libs/log"
	"github.com/cometbft/cometbft/node"
	"github.com/cometbft/cometbft/p2p"
	"github.com/cometbft/cometbft/privval"
	"github.com/cometbft/cometbft/proxy"
	"github.com/cometbft/cometbft/types"
)

const idleInterval = 4 * time.Second

type idleProofApp struct {
	abci.BaseApplication
	changing bool
}

func idleAppHash(height int64, changing bool) []byte {
	var key [8]byte
	if changing {
		binary.LittleEndian.PutUint64(key[:], uint64(height))
	}
	digest := sha256.Sum256(key[:])
	return digest[:]
}

func (app idleProofApp) InitChain(context.Context, *abci.RequestInitChain) (*abci.ResponseInitChain, error) {
	return &abci.ResponseInitChain{AppHash: idleAppHash(0, app.changing)}, nil
}

func (app idleProofApp) FinalizeBlock(_ context.Context, request *abci.RequestFinalizeBlock) (*abci.ResponseFinalizeBlock, error) {
	return &abci.ResponseFinalizeBlock{AppHash: idleAppHash(request.Height, app.changing)}, nil
}

func startIdleNode(t *testing.T, changing bool) (*node.Node, *types.GenesisDoc) {
	t.Helper()
	root := t.TempDir()
	config.EnsureRoot(root)
	cfg := config.DefaultConfig().SetRoot(root)
	cfg.ProxyApp = "local"
	cfg.RPC.ListenAddress = ""
	cfg.P2P.ListenAddress = "tcp://127.0.0.1:0"
	cfg.P2P.PexReactor = false
	cfg.Consensus.CreateEmptyBlocks = false
	cfg.Consensus.CreateEmptyBlocksInterval = idleInterval
	cfg.Consensus.TimeoutCommit = 120 * time.Millisecond
	cfg.Consensus.SkipTimeoutCommit = false

	validator := privval.LoadOrGenFilePV(cfg.PrivValidatorKeyFile(), cfg.PrivValidatorStateFile())
	publicKey, err := validator.GetPubKey()
	if err != nil {
		t.Fatal(err)
	}
	nodeKey, err := p2p.LoadOrGenNodeKey(cfg.NodeKeyFile())
	if err != nil {
		t.Fatal(err)
	}
	genesis := &types.GenesisDoc{
		ChainID:     "fern-idle-proof-fixture",
		GenesisTime: time.Now().UTC().Add(-time.Second),
		Validators:  []types.GenesisValidator{{PubKey: publicKey, Power: 1}},
	}
	if err := genesis.SaveAs(cfg.GenesisFile()); err != nil {
		t.Fatal(err)
	}
	probe, err := node.NewNode(cfg, validator, nodeKey, proxy.NewLocalClientCreator(idleProofApp{changing: changing}),
		node.DefaultGenesisDocProviderFunc(cfg), config.DefaultDBProvider,
		node.DefaultMetricsProvider(cfg.Instrumentation), log.NewNopLogger())
	if err != nil {
		t.Fatal(err)
	}
	if err := probe.Start(); err != nil {
		t.Fatal(err)
	}
	t.Cleanup(func() {
		if err := probe.Stop(); err != nil {
			t.Error(err)
		}
		probe.Wait()
	})
	return probe, genesis
}

func awaitIdleHeight(t *testing.T, probe *node.Node, height int64, timeout time.Duration) {
	t.Helper()
	ticker := time.NewTicker(5 * time.Millisecond)
	defer ticker.Stop()
	deadline := time.NewTimer(timeout)
	defer deadline.Stop()
	for probe.BlockStore().Height() < height {
		select {
		case <-ticker.C:
		case <-deadline.C:
			t.Fatalf("waited %s for height %d, reached %d", timeout, height, probe.BlockStore().Height())
		}
	}
}

func TestIdleProofBlockCadence(t *testing.T) {
	t.Run("unchanged app hash waits", func(t *testing.T) {
		probe, _ := startIdleNode(t, false)
		awaitIdleHeight(t, probe, 1, idleInterval)
		time.Sleep(750 * time.Millisecond)
		if height := probe.BlockStore().Height(); height != 1 {
			t.Fatalf("expected idle height 1 before the configured interval, got %d", height)
		}
		t.Log("unchanged AppHash: height remained 1 during a 750 ms idle observation")
	})

	t.Run("changed app hash needs successive proof blocks", func(t *testing.T) {
		probe, genesis := startIdleNode(t, true)
		awaitIdleHeight(t, probe, 1, idleInterval)
		started := time.Now()
		awaitIdleHeight(t, probe, 6, idleInterval)
		elapsed := time.Since(started)
		store := probe.BlockStore()
		validators := types.NewValidatorSet([]*types.Validator{
			types.NewValidator(genesis.Validators[0].PubKey, genesis.Validators[0].Power),
		})
		for height := int64(1); height <= 4; height++ {
			block := store.LoadBlock(height + 1)
			metadata := store.LoadBlockMeta(height + 1)
			commit := store.LoadBlockCommit(height + 1)
			if block == nil || metadata == nil || commit == nil {
				t.Fatalf("missing H+1 proof material at H=%d", height)
			}
			if len(block.Txs) != 0 || !bytes.Equal(block.AppHash, idleAppHash(height, true)) {
				t.Fatalf("wrong H+1 AppHash or nonempty block at H=%d", height)
			}
			if !bytes.Equal(block.Hash(), metadata.BlockID.Hash) {
				t.Fatalf("H+1 Header identity mismatch at H=%d", height)
			}
			if err := validators.VerifyCommit(genesis.ChainID, metadata.BlockID, height+1, commit); err != nil {
				t.Fatalf("H+1 commit at H=%d: %v", height, err)
			}
		}
		t.Logf("changing AppHash: 5 successor blocks in %s with a %s idle interval, verified four H+1 commits", elapsed, idleInterval)
	})
}
