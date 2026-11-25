// Sends bounded batches of signed transactions to real CometBFT RPC endpoints.
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
	"net/http"
	"net/url"
	"os"
	"strconv"
	"strings"
	"sync"
	"time"
)

type transaction struct {
	ID      string
	Request json.RawMessage
}

type admission struct {
	ID        string   `json:"id,omitempty"`
	Node      *int     `json:"node,omitempty"`
	Scheduled float64  `json:"scheduled_s"`
	Offered   *float64 `json:"offered_s,omitempty"`
	Ack       *float64 `json:"ack_s,omitempty"`
	Code      *int     `json:"code,omitempty"`
	Log       string   `json:"log,omitempty"`
	Error     string   `json:"error,omitempty"`
	Dropped   bool     `json:"client_dropped,omitempty"`
}

type reply struct {
	ID     int `json:"id"`
	Result *struct {
		Code int    `json:"code"`
		Log  string `json:"log"`
		Hash string `json:"hash"`
	} `json:"result"`
	Error json.RawMessage `json:"error"`
}

type rpcReplies []reply

func (replies *rpcReplies) UnmarshalJSON(data []byte) error {
	if len(data) > 0 && data[0] == '{' {
		var response reply
		if err := json.Unmarshal(data, &response); err != nil {
			return err
		}
		*replies = rpcReplies{response}
		return nil
	}
	return json.Unmarshal(data, (*[]reply)(replies))
}

type batch struct {
	Start int
	End   int
	Node  int
}

func readTransactions(path string) ([]transaction, error) {
	var fixture struct {
		Transactions []string `json:"transactions"`
	}
	f, err := os.Open(path)
	if err != nil {
		return nil, err
	}
	defer f.Close()
	if err := json.NewDecoder(io.LimitReader(f, 256<<20)).Decode(&fixture); err != nil {
		return nil, err
	}
	if len(fixture.Transactions) == 0 || len(fixture.Transactions) > 262144 {
		return nil, errors.New("invalid transaction count")
	}
	out := make([]transaction, len(fixture.Transactions))
	for i, encoded := range fixture.Transactions {
		payload, err := hex.DecodeString(strings.TrimPrefix(encoded, "0x"))
		if err != nil {
			return nil, err
		}
		id := sha256.Sum256(payload)
		req := struct {
			JSONRPC string            `json:"jsonrpc"`
			ID      int               `json:"id"`
			Method  string            `json:"method"`
			Params  map[string][]byte `json:"params"`
		}{"2.0", i + 1, "broadcast_tx_sync", map[string][]byte{"tx": payload}}
		body, err := json.Marshal(req)
		if err != nil {
			return nil, err
		}
		out[i] = transaction{fmt.Sprintf("%x", id), body}
	}
	return out, nil
}

func submit(client *http.Client, endpoints []string, txs []transaction, rows []admission, job batch, begin time.Time) {
	requests := make([]json.RawMessage, 0, job.End-job.Start)
	for i := job.Start; i < job.End; i++ {
		requests = append(requests, txs[i].Request)
	}
	body, err := json.Marshal(requests)
	offered := time.Since(begin).Seconds()
	var replies rpcReplies
	if err == nil {
		var response *http.Response
		response, err = client.Post(endpoints[job.Node], "application/json", bytes.NewReader(body))
		if err == nil {
			if response.StatusCode != http.StatusOK {
				err = fmt.Errorf("HTTP status %d", response.StatusCode)
			} else {
				err = json.NewDecoder(io.LimitReader(response.Body, 4<<20)).Decode(&replies)
			}
			response.Body.Close()
		}
	}
	ack := time.Since(begin).Seconds()
	for i := job.Start; i < job.End; i++ {
		r := &rows[i]
		r.ID, r.Node, r.Offered, r.Ack = txs[i].ID, &job.Node, &offered, &ack
		if err != nil {
			r.Error = err.Error()
		} else {
			r.Error = "missing RPC response"
		}
	}
	if err != nil {
		return
	}
	for _, response := range replies {
		i := response.ID - 1
		if i < job.Start || i >= job.End {
			continue
		}
		r := &rows[i]
		if response.Result == nil {
			r.Error = string(response.Error)
			continue
		}
		if response.Result.Code == 0 && !strings.EqualFold(response.Result.Hash, r.ID) {
			r.Error = "CometBFT returned a different transaction ID"
			continue
		}
		r.Code, r.Log, r.Error = &response.Result.Code, response.Result.Log, ""
	}
}

func rpcEndpoints(ports, addresses string) ([]string, error) {
	if (ports == "") == (addresses == "") {
		return nil, errors.New("provide either -ports or -rpc-urls")
	}
	if ports != "" {
		values := strings.Split(ports, ",")
		for i, value := range values {
			port, err := strconv.Atoi(value)
			if err != nil || port <= 0 || port > 65535 {
				return nil, errors.New("invalid RPC port")
			}
			values[i] = "http://127.0.0.1:" + strconv.Itoa(port) + "/"
		}
		return values, nil
	}
	values := strings.Split(addresses, ",")
	for _, value := range values {
		endpoint, err := url.Parse(value)
		if err != nil || endpoint.Hostname() == "" || (endpoint.Scheme != "http" && endpoint.Scheme != "https") ||
			endpoint.User != nil || endpoint.RawQuery != "" || endpoint.ForceQuery || endpoint.Fragment != "" {
			return nil, errors.New("RPC URLs must use HTTP(S) without credentials, queries or fragments")
		}
	}
	return values, nil
}

func run() error {
	workload := flag.String("workload", "", "fixture JSON")
	rpcPorts := flag.String("ports", "", "comma-delimited loopback CometBFT RPC ports")
	rpcURLs := flag.String("rpc-urls", "", "comma-delimited HTTP(S) CometBFT RPC URLs")
	start := flag.Int64("start-ns", 0, "Unix start time in nanoseconds")
	rate := flag.Float64("rate", 0, "scheduled transactions per second")
	workers := flag.Int("workers", 16, "parallel RPC requests")
	size := flag.Int("batch", 8, "transactions per JSON-RPC request")
	burst := flag.Int("burst", 0, "simultaneously scheduled transactions per burst")
	flag.Parse()
	if *start == 0 || *rate <= 0 || math.IsNaN(*rate) || math.IsInf(*rate, 0) ||
		*workers < 1 || *workers > 512 || *size < 1 || *size > 64 || *burst < 0 || flag.NArg() != 0 {
		return errors.New("invalid load parameters")
	}
	endpoints, err := rpcEndpoints(*rpcPorts, *rpcURLs)
	if err != nil {
		return err
	}
	txs, err := readTransactions(*workload)
	if err != nil {
		return err
	}
	now := time.Now()
	begin := now.Add(time.Unix(0, *start).Sub(now))
	client := &http.Client{
		Timeout:   5 * time.Second,
		Transport: &http.Transport{MaxIdleConns: *workers * len(endpoints), MaxIdleConnsPerHost: *workers, ForceAttemptHTTP2: true},
		CheckRedirect: func(*http.Request, []*http.Request) error {
			return http.ErrUseLastResponse
		},
	}
	defer client.CloseIdleConnections()
	rows := make([]admission, len(txs))
	for i := range rows {
		n := i
		if *burst > 0 {
			n -= n % *burst
		}
		rows[i].Scheduled = float64(n) / *rate
	}
	var wait sync.WaitGroup
	jobs := make(chan batch, *workers*4)
	for range *workers {
		wait.Add(1)
		go func() {
			defer wait.Done()
			for job := range jobs {
				submit(client, endpoints, txs, rows, job, begin)
			}
		}()
	}
	for i := 0; i < len(txs); {
		end := min(i+*size, len(txs))
		if *burst > 0 {
			end = min(end, i+*burst-i%*burst)
		}
		time.Sleep(time.Until(begin.Add(time.Duration(rows[end-1].Scheduled * float64(time.Second)))))
		job := batch{i, end, (i / *size) % len(endpoints)}
		select {
		case jobs <- job:
		default:
			for j := i; j < end; j++ {
				rows[j].Dropped = true
			}
		}
		i = end
	}
	close(jobs)
	wait.Wait()
	return json.NewEncoder(os.Stdout).Encode(rows)
}

func main() {
	if err := run(); err != nil {
		fmt.Fprintln(os.Stderr, err)
		os.Exit(1)
	}
}
