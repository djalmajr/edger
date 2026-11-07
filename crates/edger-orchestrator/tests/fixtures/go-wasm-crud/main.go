// crates/edger-orchestrator/tests/fixtures/go-wasm-crud/main.go

// Package main: TEST-ONLY Go/WASM fixture (EDG-54 part B). It is derived from
// workers/examples/go-wasm-http (abi.go is a verbatim copy) and adds the
// smallest write/read surface needed to prove a managed database end to end
// through the real binary. It is never installed as a product worker and the
// approved example artifact is not touched.
//
// Routes (every SQL statement is a constant in this file; nothing the visitor
// sends ever reaches SQL):
//
//	POST /notes/init — creates the notes table if it is missing
//	POST /notes      — inserts one row whose value is fixed in this file
//	GET  /notes      — {"count": <rows>, "last": "<value of the newest row>"}
//	anything else    — 404
//
// Build (Windows Go, stdlib only, no network):
//
//	GOOS=wasip1 GOARCH=wasm GOTOOLCHAIN=local GOWORK=off GOPROXY=off \
//	  go build -trimpath -buildmode=c-shared -ldflags='-s -w' -o go-wasm-crud.wasm .

package main

import (
	"bytes"
	"encoding/json"
	"io"
	"net/http"
	"net/url"
	"os"
	"strconv"
	"strings"
)

var (
	staticBody400 = []byte(`{"error":"bad request"}`)
	staticBody404 = []byte(`{"error":"not found"}`)
	staticBody405 = []byte(`{"error":"method not allowed"}`)
	staticBody413 = []byte(`{"error":"response too large"}`)
	staticBody500 = []byte(`{"error":"internal"}`)
	staticBody502 = []byte(`{"error":"upstream error"}`)
	staticBody503 = []byte(`{"error":"configuration missing"}`)
	staticBodyOK  = []byte(`{"ok":true}`)
)

// The only statements this module ever sends.
const (
	sqlInit   = "CREATE TABLE IF NOT EXISTS notes (id INTEGER PRIMARY KEY AUTOINCREMENT, body TEXT NOT NULL)"
	sqlInsert = "INSERT INTO notes(body) VALUES ('go-crud-note')"
	sqlRead   = "SELECT count(*), COALESCE((SELECT body FROM notes ORDER BY id DESC LIMIT 1), '') FROM notes"
)

const dbPipelinePath = "v2/pipeline"

var handler = http.HandlerFunc(dispatch)

func main() {}

func dispatch(w http.ResponseWriter, r *http.Request) {
	switch r.URL.Path {
	case "/notes/init":
		if r.Method != http.MethodPost {
			writeStatic(w, http.StatusMethodNotAllowed, staticBody405)
			return
		}
		if _, ok := runSQL(w, sqlInit); ok {
			writeStatic(w, http.StatusOK, staticBodyOK)
		}
	case "/notes":
		switch r.Method {
		case http.MethodPost:
			if _, ok := runSQL(w, sqlInsert); ok {
				writeStatic(w, http.StatusOK, staticBodyOK)
			}
		case http.MethodGet:
			rows, ok := runSQL(w, sqlRead)
			if !ok {
				return
			}
			if len(rows) != 1 || len(rows[0]) != 2 || rows[0][0].Type != "integer" {
				writeStatic(w, http.StatusBadGateway, staticBody502)
				return
			}
			count, err := strconv.ParseInt(rows[0][0].Value, 10, 64)
			if err != nil {
				writeStatic(w, http.StatusBadGateway, staticBody502)
				return
			}
			body, err := json.Marshal(map[string]any{"count": count, "last": rows[0][1].Value})
			if err != nil {
				writeStatic(w, http.StatusInternalServerError, staticBody500)
				return
			}
			writeStatic(w, http.StatusOK, body)
		default:
			writeStatic(w, http.StatusMethodNotAllowed, staticBody405)
		}
	default:
		writeStatic(w, http.StatusNotFound, staticBody404)
	}
}

type dbCell struct {
	Type  string `json:"type"`
	Value string `json:"value"`
}

type dbPipelineResult struct {
	Type     string `json:"type"`
	Response struct {
		Result struct {
			Rows [][]dbCell `json:"rows"`
		} `json:"result"`
	} `json:"response"`
}

type dbPipelineResponse struct {
	Results []dbPipelineResult `json:"results"`
}

// runSQL executes one constant statement (then closes) through the host
// import. On any failure it has already written a static answer and returns
// ok=false; the upstream body, error text, URL and token never leave here.
func runSQL(w http.ResponseWriter, sql string) ([][]dbCell, bool) {
	token := os.Getenv("SQLD_TOKEN")
	base := os.Getenv("SQLD_URL")
	if token == "" || !validServiceBase(base) {
		writeStatic(w, http.StatusServiceUnavailable, staticBody503)
		return nil, false
	}
	endpoint := base
	if !strings.HasSuffix(endpoint, "/") {
		endpoint += "/"
	}
	endpoint += dbPipelinePath
	payload, err := json.Marshal(map[string]any{"requests": []any{
		map[string]any{"type": "execute", "stmt": map[string]any{"sql": sql}},
		map[string]any{"type": "close"},
	}})
	if err != nil {
		writeStatic(w, http.StatusInternalServerError, staticBody500)
		return nil, false
	}
	out, err := http.NewRequest(http.MethodPost, endpoint, bytes.NewReader(payload))
	if err != nil {
		writeStatic(w, http.StatusBadGateway, staticBody502)
		return nil, false
	}
	out.Header.Set("Content-Type", "application/json")
	out.Header.Set("Authorization", "Bearer "+token)
	client := &http.Client{
		Transport: egressRoundTripper{},
		CheckRedirect: func(*http.Request, []*http.Request) error {
			return http.ErrUseLastResponse
		},
	}
	resp, err := client.Do(out)
	if err != nil {
		writeStatic(w, http.StatusBadGateway, staticBody502)
		return nil, false
	}
	defer resp.Body.Close()
	if resp.StatusCode < 200 || resp.StatusCode >= 300 {
		writeStatic(w, http.StatusBadGateway, staticBody502)
		return nil, false
	}
	respBody, err := io.ReadAll(io.LimitReader(resp.Body, maxResponseBodyBytes+1))
	if err != nil || int64(len(respBody)) > maxResponseBodyBytes {
		writeStatic(w, http.StatusBadGateway, staticBody502)
		return nil, false
	}
	var parsed dbPipelineResponse
	if err := json.Unmarshal(respBody, &parsed); err != nil || len(parsed.Results) < 2 ||
		parsed.Results[0].Type != "ok" || parsed.Results[1].Type != "ok" {
		writeStatic(w, http.StatusBadGateway, staticBody502)
		return nil, false
	}
	return parsed.Results[0].Response.Result.Rows, true
}

func validServiceBase(base string) bool {
	if base == "" {
		return false
	}
	u, err := url.Parse(base)
	if err != nil {
		return false
	}
	if u.Scheme != "http" && u.Scheme != "https" {
		return false
	}
	if u.Host == "" || u.User != nil || u.Fragment != "" || u.RawQuery != "" {
		return false
	}
	return u.Path == "" || u.Path == "/"
}

func writeStatic(w http.ResponseWriter, status int, body []byte) {
	h := w.Header()
	h.Set("Content-Type", "application/json")
	h.Set("Content-Length", strconv.Itoa(len(body)))
	w.WriteHeader(status)
	_, _ = w.Write(body)
}
