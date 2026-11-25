package main

import (
	"bytes"
	"crypto/sha256"
	"encoding/binary"
	"encoding/hex"
	"encoding/json"
	"errors"
	"fmt"
	"os"
	"strings"

	"github.com/cometbft/cometbft/crypto/merkle"
	cmtbytes "github.com/cometbft/cometbft/libs/bytes"
	cmtjson "github.com/cometbft/cometbft/libs/json"
	core "github.com/cometbft/cometbft/rpc/core/types"
	"github.com/cometbft/cometbft/types"
)

const (
	outPointLength = ckbHashSize + 4
	maxCellRecord  = 16 << 20
	maxImage       = 8 << 30
)

type proofPath struct {
	Index int64               `json:"index"`
	Total int64               `json:"total"`
	Aunts []cmtbytes.HexBytes `json:"aunts"`
}

type membership struct {
	OutPoint  cmtbytes.HexBytes `json:"out_point"`
	Record    []byte            `json:"record"`
	GroupRoot cmtbytes.HexBytes `json:"group_root"`
	CellPath  proofPath         `json:"cell_path"`
	GroupPath proofPath         `json:"group_path"`
}

type checkpointState struct {
	Format        uint32            `json:"format"`
	Height        int64             `json:"height"`
	CKBHeaderHash cmtbytes.HexBytes `json:"ckb_header_hash"`
	ImageSize     uint64            `json:"image_size"`
	ImageDigest   cmtbytes.HexBytes `json:"image_digest"`
	CellsRoot     cmtbytes.HexBytes `json:"cells_root"`
	Membership    membership        `json:"membership"`
}

type checkpointProof struct {
	Checkpoint checkpointState `json:"checkpoint"`
	Commit     json.RawMessage `json:"commit"`
	NextCommit json.RawMessage `json:"next_commit"`
	Handoffs   json.RawMessage `json:"handoffs,omitempty"`
}

func (p proofPath) verify(root, item []byte, limit int64) error {
	if p.Total < 1 || p.Total > limit || p.Index < 0 || p.Index >= p.Total {
		return errors.New("invalid Live Cell Merkle position")
	}
	aunts := make([][]byte, len(p.Aunts))
	for i, hash := range p.Aunts {
		aunts[i] = hash
	}
	hash := sha256.New()
	hash.Write([]byte{0})
	hash.Write(item)
	path := merkle.Proof{Total: p.Total, Index: p.Index, LeafHash: hash.Sum(nil), Aunts: aunts}
	if err := path.ValidateBasic(); err != nil {
		return err
	}
	return path.Verify(root, item)
}

func (state checkpointState) commitment() []byte {
	hash := sha256.New()
	switch state.Format {
	case 4:
		hash.Write([]byte("CKB sidechain authenticated database snapshot v4\x00"))
	case 3:
		hash.Write([]byte("CKB sidechain authenticated database snapshot v3\x00"))
	default:
		hash.Write([]byte("CKB sidechain authenticated database snapshot v2\x00"))
	}
	hash.Write(binary.LittleEndian.AppendUint64(nil, uint64(state.Height)))
	hash.Write(state.CKBHeaderHash)
	hash.Write(binary.LittleEndian.AppendUint64(nil, state.ImageSize))
	hash.Write(state.ImageDigest)
	hash.Write(state.CellsRoot)
	return hash.Sum(nil)
}

func (state checkpointState) verifyCell(outpoint, appHash []byte) error {
	cell := state.Membership
	if state.Format != 0 && (state.Format < 2 || state.Format > 4) {
		return errors.New("unsupported Live Cell checkpoint format")
	}
	if len(state.CKBHeaderHash) != ckbHashSize || len(state.ImageDigest) != ckbHashSize ||
		len(state.CellsRoot) != ckbHashSize || state.ImageSize == 0 || state.ImageSize > maxImage {
		return errors.New("invalid Live Cell checkpoint manifest")
	}
	if !bytes.Equal(state.commitment(), appHash) {
		return errors.New("checkpoint commitment differs from the signed successor")
	}
	if !bytes.Equal(outpoint, cell.OutPoint) || len(cell.GroupRoot) != ckbHashSize ||
		len(cell.Record) < 40 || len(cell.Record) > maxCellRecord {
		return errors.New("invalid Live Cell record or different OutPoint")
	}
	if err := cell.GroupPath.verify(state.CellsRoot, cell.GroupRoot, maxImage/45); err != nil {
		return fmt.Errorf("invalid Live Cell group path: %w", err)
	}
	item := make([]byte, 0, outPointLength+len(cell.Record))
	item = append(item, cell.OutPoint...)
	item = append(item, cell.Record...)
	if err := cell.CellPath.verify(cell.GroupRoot, item, 128); err != nil {
		return fmt.Errorf("invalid Live Cell path: %w", err)
	}
	return nil
}

func verifyCellFile(genesis *types.GenesisDoc, path, handoffsPath string, height int64, expectedPoint string) error {
	point, err := hex.DecodeString(strings.TrimPrefix(expectedPoint, "0x"))
	if err != nil || len(point) != outPointLength || height < 1 {
		return errors.New("-cell-proof requires positive -height and a 36-byte -out-point")
	}
	data, err := readLimited(path)
	if err != nil {
		return err
	}
	var proof checkpointProof
	if err := json.Unmarshal(data, &proof); err != nil {
		return err
	}
	var first, next core.ResultCommit
	if err := cmtjson.Unmarshal(proof.Commit, &first); err != nil {
		return err
	}
	if err := cmtjson.Unmarshal(proof.NextCommit, &next); err != nil {
		return err
	}
	if handoffsPath != "" {
		if len(proof.Handoffs) != 0 {
			return errors.New("supply committee handoffs in the bundle or with -handoffs")
		}
		proof.Handoffs, err = readLimited(handoffsPath)
		if err != nil {
			return err
		}
	}
	var handoffs []committeeHandoff
	if len(proof.Handoffs) != 0 {
		if err := cmtjson.Unmarshal(proof.Handoffs, &handoffs); err != nil {
			return err
		}
	}
	state := proof.Checkpoint
	if state.Height != height || first.Header == nil || first.Height != height ||
		!bytes.Equal(first.DataHash, merkle.HashFromByteSlices(nil)) {
		return errors.New("certificate does not identify the expected empty checkpoint")
	}
	if err := verifySuccessor(genesis, first.SignedHeader, next.SignedHeader, handoffs); err != nil {
		return err
	}
	if err := state.verifyCell(point, next.AppHash); err != nil {
		return err
	}
	record := state.Membership.Record
	size := int(binary.LittleEndian.Uint32(record[36:40]))
	if size < 16 || size > len(record)-36 {
		return errors.New("invalid packed CellOutput length")
	}
	return json.NewEncoder(os.Stdout).Encode(map[string]any{
		"chain_id":                 genesis.ChainID,
		"height":                   height,
		"checkpoint_header_hash":   fmt.Sprintf("0x%x", []byte(state.CKBHeaderHash)),
		"app_hash":                 fmt.Sprintf("0x%x", []byte(next.AppHash)),
		"out_point":                fmt.Sprintf("0x%x", point),
		"creation_ckb_header_hash": fmt.Sprintf("0x%x", record[:32]),
		"transaction_index":        binary.LittleEndian.Uint32(record[32:36]),
		"cell_output":              fmt.Sprintf("0x%x", record[36:36+size]),
		"data":                     fmt.Sprintf("0x%x", record[36+size:]),
	})
}
