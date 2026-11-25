package main

import (
	"bytes"
	"context"
	"encoding/hex"
	"encoding/json"
	"errors"
	"fmt"
	"io"
	"math"
	"net/http"
	"net/url"
	"strconv"
	"strings"
	"time"

	cmtjson "github.com/cometbft/cometbft/libs/json"
	core "github.com/cometbft/cometbft/rpc/core/types"
	"github.com/cometbft/cometbft/types"
)

const maxExportResponse = 8 << 20
const maxBlockTransactions = 4096

func exportExecutionProof(genesis *types.GenesisDoc, address string, height int64, hash, handoffFile string, out io.Writer) error {
	expected, err := hex.DecodeString(strings.TrimPrefix(hash, "0x"))
	if err != nil || len(expected) != ckbHashSize || height < 1 || height == math.MaxInt64 {
		return errors.New("-rpc requires a positive transaction -height with a successor and a 32-byte -ckb-tx")
	}
	endpoint, err := url.Parse(address)
	if err != nil || endpoint.Host == "" || (endpoint.Scheme != "http" && endpoint.Scheme != "https") ||
		endpoint.User != nil || endpoint.RawQuery != "" || endpoint.Fragment != "" {
		return errors.New("-rpc requires an HTTP(S) URL without credentials, query or fragment")
	}
	var proof executionProof
	if handoffFile != "" {
		data, err := readLimited(handoffFile)
		if err != nil {
			return err
		}
		if err := cmtjson.Unmarshal(data, &proof.Handoffs); err != nil {
			return err
		}
	}

	ctx, cancel := context.WithTimeout(context.Background(), 15*time.Second)
	defer cancel()
	client := &http.Client{CheckRedirect: func(*http.Request, []*http.Request) error {
		return http.ErrUseLastResponse
	}}
	var block core.ResultBlock
	for _, query := range []struct {
		method string
		height int64
		result any
	}{
		{"block", height, &block},
		{"block_results", height, &proof.Results},
		{"commit", height, &proof.Commit},
		{"commit", height + 1, &proof.NextCommit},
	} {
		if err := fetchAtHeight(ctx, client, endpoint, query.method, query.height, query.result); err != nil {
			return fmt.Errorf("%s at height %d: %w", query.method, query.height, err)
		}
	}
	if block.Block == nil || block.Block.Height != height || proof.Results.Height != height ||
		len(block.Block.Txs) != len(proof.Results.TxsResults) || len(block.Block.Txs) > maxBlockTransactions {
		return errors.New("block and execution results differ from the requested height or transaction count")
	}
	index := -1
	for i, result := range proof.Results.TxsResults {
		if result != nil && result.Code == 0 && len(result.Data) >= ckbHashSize &&
			bytes.Equal(result.Data[:ckbHashSize], expected) {
			if index != -1 {
				return errors.New("block returned multiple receipts for the requested CKB transaction")
			}
			index = i
		}
	}
	if index == -1 {
		return errors.New("block has no successful receipt for the requested CKB transaction")
	}
	txs := block.Block.Txs
	proof.Tx = core.ResultTx{
		Hash: txs[index].Hash(), Height: height, Index: uint32(index), Tx: txs[index],
		TxResult: *proof.Results.TxsResults[index], Proof: txs.Proof(index),
	}
	if err := verify(genesis, &proof); err != nil {
		return err
	}
	encoded, err := cmtjson.Marshal(proof)
	if err != nil {
		return err
	}
	if len(encoded) >= maxProofInput {
		return errors.New("proof bundle exceeds size limit")
	}
	_, err = out.Write(append(encoded, '\n'))
	return err
}

func fetchAtHeight(ctx context.Context, client *http.Client, endpoint *url.URL, method string, height int64, result any) error {
	payload, err := json.Marshal(map[string]any{
		"jsonrpc": "2.0", "id": 1, "method": method,
		"params": map[string]string{"height": strconv.FormatInt(height, 10)},
	})
	if err != nil {
		return err
	}
	request, err := http.NewRequestWithContext(ctx, http.MethodPost, endpoint.String(), bytes.NewReader(payload))
	if err != nil {
		return err
	}
	request.Header.Set("Content-Type", "application/json")
	reply, err := client.Do(request)
	if err != nil {
		return err
	}
	defer reply.Body.Close()
	if reply.StatusCode != http.StatusOK {
		return fmt.Errorf("unexpected HTTP status %d", reply.StatusCode)
	}
	data, err := io.ReadAll(io.LimitReader(reply.Body, maxExportResponse+1))
	if err != nil {
		return err
	}
	if len(data) > maxExportResponse {
		return errors.New("RPC response exceeds size limit")
	}
	var envelope struct {
		Version string          `json:"jsonrpc"`
		ID      int             `json:"id"`
		Result  json.RawMessage `json:"result"`
		Error   *struct {
			Message string `json:"message"`
			Data    string `json:"data"`
		} `json:"error"`
	}
	if err := json.Unmarshal(data, &envelope); err != nil {
		return err
	}
	if envelope.Version != "2.0" || envelope.ID != 1 {
		return errors.New("unexpected JSON-RPC response envelope")
	}
	if envelope.Error != nil {
		return fmt.Errorf("%s: %s", envelope.Error.Message, envelope.Error.Data)
	}
	return cmtjson.Unmarshal(envelope.Result, result)
}
