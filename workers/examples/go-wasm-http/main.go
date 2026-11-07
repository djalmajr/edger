// workers/examples/go-wasm-http/main.go

// Package main: the go-wasm-http example worker — real Go on wasip1 over
// the EdgeR WASM HTTP ABI (ABI v1). The module is a reactor
// (-buildmode=c-shared): main() is empty and the host drives one request
// at a time through the exports in abi.go (edger_alloc/edger_handle,
// reactor _initialize emitted by the toolchain).
//
// Routes:
//
//	/inspect (and subpaths) — the fields the host preserves on the request
//	/db                    — the fixed SQLD probe through the host import
//	anything else          — 404
//
// The only outbound path is the edger_http_v1.fetch host import
// (abi.go); there is no socket, no listener and no http.DefaultTransport
// in this module.
//
// Build (Windows Go, no network dependencies — stdlib only):
//
//	GOOS=wasip1 GOARCH=wasm GOTOOLCHAIN=local GOWORK=off GOPROXY=off \
//	  go build -trimpath -buildmode=c-shared -ldflags='-s -w' -o index.wasm .

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

// Static JSON bodies: no error value, stack, URL, token or upstream body
// ever reaches a payload (the host and the caller see the class only).
var (
	staticBody400 = []byte(`{"error":"bad request"}`)
	staticBody404 = []byte(`{"error":"not found"}`)
	staticBody413 = []byte(`{"error":"response too large"}`)
	staticBody500 = []byte(`{"error":"internal"}`)
	staticBody502 = []byte(`{"error":"upstream error"}`)
	staticBody503 = []byte(`{"error":"configuration missing"}`)
	dbOKBody      = []byte(`{"ok":true,"value":1}`)
)

// handler is the single registered http.Handler: the reactor ABI
// (abi.go) serves every host request through it. No listener, no server.
var handler = http.HandlerFunc(dispatch)

// main exists only because the Go reactor needs one; the host drives the
// module through its exports, never through main.
func main() {}

func dispatch(w http.ResponseWriter, r *http.Request) {
	switch {
	case r.URL.Path == "/inspect" || strings.HasPrefix(r.URL.Path, "/inspect/"):
		inspectHandler(w, r)
	case r.URL.Path == "/db":
		dbHandler(w, r)
	default:
		writeStatic(w, http.StatusNotFound, staticBody404)
	}
}

type inspectPayload struct {
	Method     string `json:"method"`
	RequestURI string `json:"requestUri"`
	Path       string `json:"path"`
	RawPath    string `json:"rawPath"`
	RawQuery   string `json:"rawQuery"`
	Host       string `json:"host"`
	XExample   string `json:"xExample"`
	Body       []byte `json:"body"` // base64 by JSON encoding
}

// inspectHandler answers with the fields the host preserves (method,
// RequestURI, URL path/RawPath/RawQuery, Host), the single x-example
// header and the body read limited to 64KiB. It deliberately does not
// echo the rest of the headers, auth, cookies or env.
func inspectHandler(w http.ResponseWriter, r *http.Request) {
	body, err := io.ReadAll(io.LimitReader(r.Body, inspectBodyLimitBytes))
	if err != nil {
		writeStatic(w, http.StatusInternalServerError, staticBody500)
		return
	}
	if body == nil {
		body = []byte{}
	}
	payload := inspectPayload{
		Method:     r.Method,
		RequestURI: r.RequestURI,
		Path:       r.URL.Path,
		RawPath:    r.URL.RawPath,
		RawQuery:   r.URL.RawQuery,
		Host:       r.Host,
		XExample:   r.Header.Get("x-example"),
		Body:       body,
	}
	writeJSON(w, http.StatusOK, &payload)
}

const (
	dbPipelinePath = "v2/pipeline"
	// dbPipelineRequest is the fixed probe query of this example; it
	// never takes SQL from the visitor.
	dbPipelineRequest = `{"requests":[{"type":"execute","stmt":{"sql":"SELECT 1 AS value"}},{"type":"close"}]}`
)

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

// dbHandler runs the fixed SQLD probe (SELECT 1 AS value, executed and
// closed in one /v2/pipeline) through the host egress import and returns
// only the validated integer 1. SQLD_TOKEN comes from the per-execution
// private WASI env (secret); SQLD_URL is the non-secret service base URL.
// Missing/malformed config answers a static 503; any HTTP or protocol
// error a static 502 — never the upstream body, error text, URL or token.
func dbHandler(w http.ResponseWriter, r *http.Request) {
	token := os.Getenv("SQLD_TOKEN")
	base := os.Getenv("SQLD_URL")
	if token == "" || !validServiceBase(base) {
		writeStatic(w, http.StatusServiceUnavailable, staticBody503)
		return
	}
	endpoint := base
	if !strings.HasSuffix(endpoint, "/") {
		endpoint += "/"
	}
	endpoint += dbPipelinePath
	out, err := http.NewRequest(http.MethodPost, endpoint, bytes.NewReader([]byte(dbPipelineRequest)))
	if err != nil {
		writeStatic(w, http.StatusBadGateway, staticBody502)
		return
	}
	out.Header.Set("Content-Type", "application/json")
	out.Header.Set("Authorization", "Bearer "+token)
	// Only the custom transport (the host import) is used; the pipeline
	// answers on the first response, so redirects are never followed.
	client := &http.Client{
		Transport: egressRoundTripper{},
		CheckRedirect: func(*http.Request, []*http.Request) error {
			return http.ErrUseLastResponse
		},
	}
	resp, err := client.Do(out)
	if err != nil {
		writeStatic(w, http.StatusBadGateway, staticBody502)
		return
	}
	defer resp.Body.Close()
	// Only a 2xx upstream answer is accepted: a 500 carrying valid JSON
	// is still the static 502, never the upstream body, URL or token.
	if resp.StatusCode < 200 || resp.StatusCode >= 300 {
		writeStatic(w, http.StatusBadGateway, staticBody502)
		return
	}
	respBody, err := io.ReadAll(io.LimitReader(resp.Body, maxResponseBodyBytes+1))
	if err != nil || int64(len(respBody)) > maxResponseBodyBytes {
		writeStatic(w, http.StatusBadGateway, staticBody502)
		return
	}
	var parsed dbPipelineResponse
	if err := json.Unmarshal(respBody, &parsed); err != nil || !validDBResponse(&parsed) {
		writeStatic(w, http.StatusBadGateway, staticBody502)
		return
	}
	writeStatic(w, http.StatusOK, dbOKBody)
}

// validServiceBase accepts only a http/https base with a host and nothing
// else: no userinfo, no fragment, no query, no path beyond "/".
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

// validDBResponse requires the execute and close results to be present
// and ok, and the cell of the fixed probe query to be the integer "1".
// Anything else (error entries, malformed shape, other values) is a
// static 502 on the caller side.
func validDBResponse(resp *dbPipelineResponse) bool {
	if len(resp.Results) < 2 {
		return false
	}
	if resp.Results[0].Type != "ok" || resp.Results[1].Type != "ok" {
		return false
	}
	rows := resp.Results[0].Response.Result.Rows
	if len(rows) == 0 || len(rows[0]) == 0 {
		return false
	}
	cell := rows[0][0]
	return cell.Type == "integer" && cell.Value == "1"
}

func writeJSON(w http.ResponseWriter, status int, v any) {
	b, err := json.Marshal(v)
	if err != nil {
		writeStatic(w, http.StatusInternalServerError, staticBody500)
		return
	}
	writeStatic(w, status, b)
}

func writeStatic(w http.ResponseWriter, status int, body []byte) {
	h := w.Header()
	h.Set("Content-Type", "application/json")
	h.Set("Content-Length", strconv.Itoa(len(body)))
	w.WriteHeader(status)
	// The limited writer (abi.go) owns the frame; an oversize or invalid
	// state only surfaces as its own 413/500 static, never as a leak.
	_, _ = w.Write(body)
}
