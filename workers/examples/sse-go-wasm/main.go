// workers/examples/sse-go-wasm/main.go

// Package main: the sse-go-wasm example worker — real Go on wasip1 over the
// EdgeR WASM HTTP ABI (ABI v1), used by the EDG-55 binary proof. The guest
// authorizes and publishes in SHORT executions; the host keeps the browser
// connection (nothing here is held per connection). abi.go is the unmodified
// copy of workers/examples/go-wasm-http/abi.go.
//
// Routes (also reachable under a leading /api, for the fullstack variant):
//
//	GET  /events  — authorizes with `Authorization: Bearer <token>` against
//	                the vault secret SSE_ACCESS_TOKEN and answers 200 with
//	                `x-edger-sse-subscribe: room.1`, or 401
//	POST /publish — same authorization; the JSON body
//	                {"events":[{"topic","data","event","id"}...]} (the test's
//	                control channel) becomes 1..8 `x-edger-sse-publish`
//	                directives. An empty list publishes nothing.
//	GET  /plain   — an ordinary answer, no SSE
//	GET  /slow    — ?ms=N keeps the execution busy for N ms (capped at 5000)
//	                and answers 200: the positive control of the test reader
//
// Test-only controls: each `topics=` query parameter of /events becomes one
// `x-edger-sse-subscribe` header (instead of the default topic) and the `raw`
// list of the /publish body is appended verbatim as extra
// `x-edger-sse-publish` values.
//	anything else — 404
//
// Build (Windows Go, no network dependencies — stdlib only):
//
//	GOOS=wasip1 GOARCH=wasm GOTOOLCHAIN=local GOWORK=off GOPROXY=off \
//	  go build -trimpath -buildmode=c-shared -ldflags='-s -w' -o index.wasm .

package main

import (
	"crypto/subtle"
	"encoding/base64"
	"encoding/json"
	"io"
	"net/http"
	"os"
	"strconv"
	"strings"
	"time"
)

var (
	staticBody400 = []byte(`{"error":"bad request"}`)
	staticBody401 = []byte(`{"error":"unauthorized"}`)
	staticBody404 = []byte(`{"error":"not found"}`)
	staticBody413 = []byte(`{"error":"response too large"}`)
	staticBody500 = []byte(`{"error":"internal"}`)
)

const (
	subscribeTopic   = "room.1"
	controlBodyLimit = 64 * 1024
	maxSlowMillis    = 5000
)

var handler = http.HandlerFunc(dispatch)

// main exists only because the Go reactor needs one; the host drives the
// module through its exports, never through main.
func main() {}

func dispatch(w http.ResponseWriter, r *http.Request) {
	path := r.URL.Path
	if strings.HasPrefix(path, "/api/") {
		path = path[len("/api"):]
	}
	switch {
	case path == "/events" && r.Method == http.MethodGet:
		eventsHandler(w, r)
	case path == "/publish":
		publishHandler(w, r)
	case path == "/plain":
		writeStatic(w, http.StatusOK, []byte(`{"plain":true}`))
	case path == "/slow":
		slowHandler(w, r)
	default:
		writeStatic(w, http.StatusNotFound, staticBody404)
	}
}

// authorized compares the Bearer token with the vault secret in constant
// time. An absent or empty secret never authorizes anyone.
func authorized(r *http.Request) bool {
	token := os.Getenv("SSE_ACCESS_TOKEN")
	if token == "" {
		return false
	}
	given := r.Header.Get("Authorization")
	want := "Bearer " + token
	return subtle.ConstantTimeCompare([]byte(given), []byte(want)) == 1
}

func eventsHandler(w http.ResponseWriter, r *http.Request) {
	if !authorized(r) {
		writeStatic(w, http.StatusUnauthorized, staticBody401)
		return
	}
	if topics := r.URL.Query()["topics"]; len(topics) > 0 {
		for _, topic := range topics {
			w.Header().Add("x-edger-sse-subscribe", topic)
		}
	} else {
		w.Header().Set("x-edger-sse-subscribe", subscribeTopic)
	}
	writeStatic(w, http.StatusOK, []byte(`{"subscribed":true}`))
}

// slowHandler keeps the execution busy for ?ms=N (at most maxSlowMillis) and
// answers 200, so the test reader can see a non-null activeRequest.
func slowHandler(w http.ResponseWriter, r *http.Request) {
	millis, err := strconv.Atoi(r.URL.Query().Get("ms"))
	if err != nil || millis < 0 {
		writeStatic(w, http.StatusBadRequest, staticBody400)
		return
	}
	if millis > maxSlowMillis {
		millis = maxSlowMillis
	}
	time.Sleep(time.Duration(millis) * time.Millisecond)
	writeStatic(w, http.StatusOK, []byte(`{"slow":true}`))
}

type controlEvent struct {
	Topic string `json:"topic"`
	Data  string `json:"data"`
	Event string `json:"event,omitempty"`
	ID    string `json:"id,omitempty"`
}

type controlBody struct {
	Events []controlEvent `json:"events"`
	Raw    []string       `json:"raw"`
}

func publishHandler(w http.ResponseWriter, r *http.Request) {
	if !authorized(r) {
		writeStatic(w, http.StatusUnauthorized, staticBody401)
		return
	}
	raw, err := io.ReadAll(io.LimitReader(r.Body, controlBodyLimit))
	if err != nil {
		writeStatic(w, http.StatusInternalServerError, staticBody500)
		return
	}
	var control controlBody
	if len(raw) > 0 {
		if err := json.Unmarshal(raw, &control); err != nil {
			writeStatic(w, http.StatusBadRequest, staticBody400)
			return
		}
	}
	for _, event := range control.Events {
		directive, err := json.Marshal(event)
		if err != nil {
			writeStatic(w, http.StatusInternalServerError, staticBody500)
			return
		}
		w.Header().Add("x-edger-sse-publish", base64.RawURLEncoding.EncodeToString(directive))
	}
	for _, raw := range control.Raw {
		w.Header().Add("x-edger-sse-publish", raw)
	}
	writeStatic(w, http.StatusOK, []byte(`{"published":`+strconv.Itoa(len(control.Events)+len(control.Raw))+`}`))
}

func writeStatic(w http.ResponseWriter, status int, body []byte) {
	h := w.Header()
	h.Set("Content-Type", "application/json")
	h.Set("Content-Length", strconv.Itoa(len(body)))
	w.WriteHeader(status)
	_, _ = w.Write(body)
}
