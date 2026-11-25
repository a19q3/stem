package main

import (
	"encoding/json"
	"fmt"
	"net/http"
	"net/http/httptest"
	"strings"
	"testing"
	"time"
)

func TestSubmitAcceptsSingleAndBatchRPCResponses(t *testing.T) {
	for _, count := range []int{1, 2} {
		t.Run(fmt.Sprintf("%d requests", count), func(t *testing.T) {
			txs := make([]transaction, count)
			responses := make([]string, count)
			for i := range txs {
				id := fmt.Sprintf("%064x", i+1)
				txs[i] = transaction{ID: id, Request: json.RawMessage(fmt.Sprintf(`{"id":%d}`, i+1))}
				responses[i] = fmt.Sprintf(`{"id":%d,"result":{"code":0,"hash":%q}}`, i+1, id)
			}
			body := responses[0]
			if count > 1 {
				body = "[" + strings.Join(responses, ",") + "]"
			}
			server := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
				var requests []json.RawMessage
				if err := json.NewDecoder(r.Body).Decode(&requests); err != nil || len(requests) != count {
					http.Error(w, "unexpected batch", http.StatusBadRequest)
					return
				}
				fmt.Fprint(w, body)
			}))
			defer server.Close()

			rows := make([]admission, count)
			submit(server.Client(), []string{server.URL}, txs, rows, batch{End: count}, time.Now())
			for i, row := range rows {
				if row.ID != txs[i].ID || row.Error != "" || row.Code == nil || *row.Code != 0 || row.Ack == nil {
					t.Errorf("missing successful admission for request %d: %+v", i+1, row)
				}
			}
		})
	}
}
