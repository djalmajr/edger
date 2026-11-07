// workers/examples/go-wasm-http/abi.go

// Package main: the go-wasm-http example — real Go on wasip1 over the
// EdgeR WASM HTTP ABI (ABI v1). This file is the SDK side of the
// linear-memory contract implemented by crates/edger-isolation (wasm/
// handler.rs, wasm/http.rs); the Rust sources are the authority and this
// module only follows them.
//
// Module exports (product ABI, verified against the host):
//
//	memory                 — exported by the Go toolchain
//	edger_alloc(i32)->i32  — reserve guest memory for one request frame
//	edger_handle(i32,i32)->i64 — run one request; packed return (len<<32)|ptr
//	_initialize()         — emitted by the Go reactor (-buildmode=c-shared);
//	                       the host runs it once per instance before the ABI
//
// Module import (EDG-52 07a, exact signature 4*i32 -> i64):
//
//	edger_http_v1.fetch(reqPtr, reqLen, outPtr, outCap) int64
//
// Request frame (host -> guest), 16-byte LE prefix:
//
//	[u32 methodLen][u32 uriLen][u32 headersLen][u32 bodyLen]
//	[method][uri][headers JSON pairs][body]
//
// Response frame (guest -> host), 12-byte LE prefix:
//
//	[u16 status 100..=599][u16 pad=0][u32 headersLen][u32 bodyLen]
//	[headers JSON pairs][body]
//
// The headers JSON is the wire shape [["name","value"],...]; an empty set
// serializes as [] (never null). Caps mirror the host (wire.rs,
// handler.rs, http.rs): request frame 256KiB, headers JSON 64KiB,
// response body 4MiB (SDK-explicit), 100 headers, 8KiB per value, 64KiB
// of name+value bytes total.

package main

import (
	"bytes"
	"encoding/binary"
	"encoding/json"
	"errors"
	"fmt"
	"io"
	"net/http"
	"net/url"
	"runtime"
	"strings"
	"unsafe"
)

const (
	requestPrefixBytes    = 16
	responsePrefixBytes   = 12
	minResponseFrameBytes = responsePrefixBytes + 2 // 12 + `[]`
	maxRequestFrameBytes  = 256 * 1024              // host cap for one inbound frame
	maxHeaderJSONBytes    = 64 * 1024               // host cap for the headers JSON
	maxResponseBodyBytes  = 4 * 1024 * 1024         // SDK-explicit response body cap
	maxEgressFrameBytes   = responsePrefixBytes + maxHeaderJSONBytes + maxResponseBodyBytes
	maxHeaderCount        = 100
	maxHeaderTotalBytes   = 64 * 1024
	maxHeaderValueBytes   = 8 * 1024
	inspectBodyLimitBytes = 64 * 1024
)

var jsonHeader = [][2]string{{"Content-Type", "application/json"}}

// edgerFetch is the exact host import of EDG-52 07a: the guest encodes
// the request frame at (reqPtr, reqLen); the host validates it, writes
// the full response frame into (outPtr, outCap) and returns the bytes
// written (>= 14) or the stable negative code: -1 unavailable, -2
// invalid request, -3 response too large, -4 transport. Nothing is
// written to the out buffer on a negative outcome.
//
//go:wasmimport edger_http_v1 fetch
func edgerFetch(reqPtr, reqLen, outPtr, outCap int32) int64

// Static egress errors: the stable class only — no URL, header value,
// credential or body fragment ever reaches the error text.
var (
	errEgressUnavailable   = errors.New("egress unavailable")
	errEgressInvalid       = errors.New("egress request invalid")
	errEgressResponseLarge = errors.New("egress response too large")
	errEgressTransport     = errors.New("egress transport error")
	errEgressCancelled     = errors.New("egress request cancelled")
	errEgressProtocol      = errors.New("egress protocol error")
	errEgressRequestLarge  = errors.New("egress request too large")
	errEgressNoURL         = errors.New("egress url invalid")
)

// The host copies each frame before the next call, so one slot per role
// keeps the Go-heap buffers alive exactly as long as the host needs
// them (the host is synchronous between alloc/write/handle, and reads
// the response frame before dispatching the next request).
var (
	requestSlot  []byte // buffer edger_alloc handed to the host, awaiting edger_handle
	responseSlot []byte // frame of the last edger_handle, awaiting the host read
)

// Edger_alloc is the product export the host calls to reserve guest
// memory for one request frame (at most the host request cap).
//
//go:wasmexport edger_alloc
func Edger_alloc(length int32) int32 {
	if length <= 0 || length > maxRequestFrameBytes {
		return 0
	}
	buf := make([]byte, int(length))
	requestSlot = buf
	// No pointer mask: the host caps the module memory at 512MiB, so the
	// address is below 2^31 and the i32 stays non-negative as the host's
	// checked_guest_ptr requires.
	return int32(uint32(uintptr(unsafe.Pointer(unsafe.SliceData(buf)))))
}

// Edger_handle is the product export: the host has written the request
// frame at (requestPtr, requestLen) and reads the response frame from
// the packed (len<<32)|ptr result. Errors and panics answer a static
// 500/400 frame — no error value or stack ever reaches the frame.
//
//go:wasmexport edger_handle
func Edger_handle(requestPtr, requestLen int32) (result int64) {
	defer func() {
		if recover() != nil {
			result = finishResponse(http.StatusInternalServerError, jsonHeader, staticBody500)
		}
	}()
	frame, ok := requestFrameAt(requestPtr, requestLen)
	if !ok {
		return finishResponse(http.StatusBadRequest, jsonHeader, staticBody400)
	}
	method, uri, pairs, body, ok := parseRequestFrame(frame)
	if !ok {
		return finishResponse(http.StatusBadRequest, jsonHeader, staticBody400)
	}
	req, ok := reconstructRequest(method, uri, pairs, body)
	if !ok {
		return finishResponse(http.StatusBadRequest, jsonHeader, staticBody400)
	}
	// No listener, no I/O: the host drives the handler in memory.
	rec := &responseWriter{}
	handler.ServeHTTP(rec, req)
	status, headers, respBody := rec.finalize()
	return finishResponse(status, headers, respBody)
}

// requestFrameAt resolves the frame the host just wrote, refusing
// pointers that are not the buffer edger_alloc handed out (the host
// writes the frame between the two imports, and nothing else in this
// module allocates through edger_alloc in between).
func requestFrameAt(requestPtr, requestLen int32) ([]byte, bool) {
	if requestPtr < 0 || requestLen < 0 || int64(requestLen) > maxRequestFrameBytes {
		return nil, false
	}
	if len(requestSlot) == 0 {
		return nil, false
	}
	if int32(uint32(uintptr(unsafe.Pointer(unsafe.SliceData(requestSlot))))) != requestPtr {
		return nil, false
	}
	if requestLen > int32(len(requestSlot)) {
		return nil, false
	}
	// The passed pointer is the slot head (checked above), so the slot
	// slice IS the frame the host wrote; no raw-pointer view is needed.
	return requestSlot[0:int(requestLen)], true
}

// parseRequestFrame decodes the product request frame with checked
// arithmetic (hostile u32 lengths overflow a 32-bit sum before they
// reach a slice) and the wire caps. Nothing is read out of bounds.
func parseRequestFrame(frame []byte) (method, uri string, headers [][2]string, body []byte, ok bool) {
	if len(frame) < requestPrefixBytes {
		return
	}
	methodLen := int64(binary.LittleEndian.Uint32(frame[0:4]))
	uriLen := int64(binary.LittleEndian.Uint32(frame[4:8]))
	headersLen := int64(binary.LittleEndian.Uint32(frame[8:12]))
	bodyLen := int64(binary.LittleEndian.Uint32(frame[12:16]))
	// The total must equal the frame exactly and stay under the host cap.
	total := int64(requestPrefixBytes) + methodLen + uriLen + headersLen + bodyLen
	if total != int64(len(frame)) || total > maxRequestFrameBytes {
		return
	}
	// The headers JSON cap is enforced BEFORE the JSON decode (host rule).
	if headersLen > maxHeaderJSONBytes {
		return
	}
	off := int64(requestPrefixBytes)
	method = string(frame[off : off+methodLen])
	off += methodLen
	uri = string(frame[off : off+uriLen])
	off += uriLen
	headersJSON := frame[off : off+headersLen]
	off += headersLen
	body = frame[off : off+bodyLen]
	if len(headersJSON) > 0 {
		if err := json.Unmarshal(headersJSON, &headers); err != nil {
			return
		}
	}
	if !validHeaderPairs(headers) {
		return
	}
	ok = true
	return
}

// reconstructRequest rebuilds the server-side http.Request from the
// frame fields. The uri is the raw Request-URI the frame carries
// verbatim (worker-relative); it is parsed with url.ParseRequestURI,
// the SERVER-target parse — never the client parser of
// http.NewRequest: an origin-form target beginning with `//` keeps its
// whole `//`-led path (a host is never fabricated from the path), while
// an absolute URI carries its own scheme and host, and its host lands
// on req.Host (overwritten by an explicit Host header); the binary body
// is preserved as-is.
func reconstructRequest(method, uri string, pairs [][2]string, body []byte) (*http.Request, bool) {
	if uri == "" {
		return nil, false
	}
	// A bodyless server request keeps a non-nil Body that returns EOF
	// (http.NoBody); a nil Body would panic the first handler read.
	var bodyReader io.Reader = http.NoBody
	if len(body) > 0 {
		bodyReader = bytes.NewReader(body)
	}
	// The fixed placeholder "/" keeps NewRequest's method validation, the
	// NoBody handling and the HTTP/1.1 proto; the original uri is never
	// passed to the client URL parser.
	req, err := http.NewRequest(method, "/", bodyReader)
	if err != nil {
		return nil, false
	}
	u, err := url.ParseRequestURI(uri)
	if err != nil {
		return nil, false
	}
	req.URL = u
	// An absolute URI carries its own host; without an explicit Host
	// header it lands on req.Host (the header loop below overwrites it
	// when the frame provides one). An origin-form target keeps Host
	// empty — a host is never fabricated from the path.
	req.Host = u.Host
	req.RequestURI = uri
	header := make(http.Header, len(pairs))
	seenHost := false
	for _, p := range pairs {
		if !seenHost && strings.EqualFold(p[0], "host") {
			seenHost = true
			req.Host = p[1]
			continue
		}
		header.Add(p[0], p[1])
	}
	req.Header = header
	if len(body) > 0 {
		req.ContentLength = int64(len(body))
	} else {
		req.ContentLength = 0
	}
	return req, true
}

// validHeaderPairs mirrors the host's validate_headers caps (wire.rs):
// at most 100 pairs, at most 8KiB per value, at most 64KiB of
// name+value bytes in total.
func validHeaderPairs(pairs [][2]string) bool {
	if len(pairs) > maxHeaderCount {
		return false
	}
	total := 0
	for _, p := range pairs {
		if len(p[1]) > maxHeaderValueBytes {
			return false
		}
		total += len(p[0]) + len(p[1])
	}
	return total <= maxHeaderTotalBytes
}

// responseWriter is the limited, buffered http.ResponseWriter the
// reactor drives: no Flusher, no Hijacker, no streaming — the host
// reads the full frame after edger_handle returns. The status defaults
// to 200 and snapshots on the first WriteHeader/Write; the body is
// capped at maxResponseBodyBytes with checked appends (an oversize
// write never counts as a partial success); header count/bytes/values
// are capped at finalize. Errors and panics end as static 500 frames
// with no error value or stack in the payload.
type responseWriter struct {
	header      http.Header
	status      int
	wroteHeader bool
	invalid     bool // WriteHeader outside 100..=599
	oversize    bool // body exceeded the cap
	body        []byte
}

func (w *responseWriter) Header() http.Header {
	if w.header == nil {
		w.header = make(http.Header)
	}
	return w.header
}

func (w *responseWriter) WriteHeader(code int) {
	if code < 100 || code > 599 {
		w.invalid = true
		return
	}
	if w.wroteHeader {
		return
	}
	w.wroteHeader = true
	w.status = code
}

func (w *responseWriter) Write(b []byte) (int, error) {
	if !w.wroteHeader {
		w.wroteHeader = true
		w.status = http.StatusOK
	}
	if w.invalid || w.oversize {
		return 0, nil
	}
	if int64(len(w.body))+int64(len(b)) > maxResponseBodyBytes {
		w.oversize = true
		return 0, nil
	}
	w.body = append(w.body, b...)
	return len(b), nil
}

// finalize folds the writer state into one valid frame: 413 static on
// oversize, 500 static on invalid status or uncapped headers, else the
// snapshot (default 200, empty header list as []).
func (w *responseWriter) finalize() (int, [][2]string, []byte) {
	if w.invalid {
		return http.StatusInternalServerError, jsonHeader, staticBody500
	}
	if w.oversize {
		return http.StatusRequestEntityTooLarge, jsonHeader, staticBody413
	}
	if !w.wroteHeader {
		w.status = http.StatusOK
	}
	pairs := [][2]string{}
	for name, values := range w.header {
		for _, v := range values {
			pairs = append(pairs, [2]string{name, v})
		}
	}
	if !validHeaderPairs(pairs) {
		return http.StatusInternalServerError, jsonHeader, staticBody500
	}
	return w.status, pairs, w.body
}

// finishResponse encodes the product response frame into responseSlot
// and packs (len<<32)|ptr, the exact shape the host's unpack_ptr_len
// expects (ptr = bits & 0xffff_ffff, len = bits >> 32). Any cap breach
// or invalid status degrades to the static 500 frame.
func finishResponse(status int, headers [][2]string, body []byte) int64 {
	if status < 100 || status > 599 {
		return finishResponseStatic()
	}
	headersJSON, err := json.Marshal(headers)
	if err != nil || len(headersJSON) > maxHeaderJSONBytes || int64(len(body)) > maxResponseBodyBytes {
		return finishResponseStatic()
	}
	return finishFrame(status, headersJSON, body)
}

// finishResponseStatic is the static 500 frame: the jsonHeader pair set
// and the tiny static body are always within the caps, so this never
// degrades further.
func finishResponseStatic() int64 {
	headersJSON, _ := json.Marshal(jsonHeader)
	return finishFrame(http.StatusInternalServerError, headersJSON, staticBody500)
}

// finishFrame writes an already-checked frame (status in range, headers
// already JSON and capped, body capped) and packs the result.
func finishFrame(status int, headersJSON, body []byte) int64 {
	frame := make([]byte, responsePrefixBytes+len(headersJSON)+len(body))
	binary.LittleEndian.PutUint16(frame[0:2], uint16(status))
	// pad (frame[2:4]) stays zero
	binary.LittleEndian.PutUint32(frame[4:8], uint32(len(headersJSON)))
	binary.LittleEndian.PutUint32(frame[8:12], uint32(len(body)))
	copy(frame[responsePrefixBytes:], headersJSON)
	copy(frame[responsePrefixBytes+len(headersJSON):], body)
	responseSlot = frame
	return int64(len(frame))<<32 | int64(uint32(uintptr(unsafe.Pointer(unsafe.SliceData(frame)))))
}

// egressRoundTripper is the only outbound path of this module: it
// encodes the request into the product frame and calls the host import
// edger_http_v1.fetch. No socket, no listener, no http.DefaultTransport,
// no net.Dial.
type egressRoundTripper struct{}

func (egressRoundTripper) RoundTrip(req *http.Request) (*http.Response, error) {
	// A context already cancelled before the call is honoured here; once
	// the import runs, the host's worker budget is the deadline authority.
	select {
	case <-req.Context().Done():
		return nil, errEgressCancelled
	default:
	}
	if req.URL == nil || req.Method == "" {
		return nil, errEgressInvalid
	}
	// Absolute http/https URL only; refuse anything else without a socket.
	if req.URL.Scheme != "http" && req.URL.Scheme != "https" || req.URL.Host == "" {
		return nil, errEgressNoURL
	}
	pairs := [][2]string{}
	for name, values := range req.Header {
		for _, v := range values {
			pairs = append(pairs, [2]string{name, v})
		}
	}
	if !validHeaderPairs(pairs) {
		return nil, errEgressRequestLarge
	}
	headersJSON, err := json.Marshal(pairs)
	if err != nil {
		return nil, errEgressProtocol
	}
	uri := req.URL.String()
	frameLen := int64(requestPrefixBytes) + int64(len(req.Method)) + int64(len(uri)) + int64(len(headersJSON))
	// Body budget so the whole frame stays under the host request cap.
	budget := int64(maxRequestFrameBytes) - frameLen
	if budget < 0 {
		return nil, errEgressRequestLarge
	}
	var body []byte
	if req.Body != nil {
		defer req.Body.Close() // the round trip owns the body; close it either way
		if req.ContentLength > 0 && req.ContentLength > budget {
			return nil, errEgressRequestLarge
		}
		var readErr error
		body, readErr = io.ReadAll(io.LimitReader(req.Body, budget+1))
		if readErr != nil {
			return nil, errEgressTransport
		}
		if int64(len(body)) > budget {
			return nil, errEgressRequestLarge
		}
	}
	frame := make([]byte, frameLen+int64(len(body)))
	binary.LittleEndian.PutUint32(frame[0:4], uint32(len(req.Method)))
	binary.LittleEndian.PutUint32(frame[4:8], uint32(len(uri)))
	binary.LittleEndian.PutUint32(frame[8:12], uint32(len(headersJSON)))
	binary.LittleEndian.PutUint32(frame[12:16], uint32(len(body)))
	off := requestPrefixBytes
	off += copy(frame[off:], req.Method)
	off += copy(frame[off:], uri)
	off += copy(frame[off:], headersJSON)
	off += copy(frame[off:], body)

	out := make([]byte, maxEgressFrameBytes) // >= minResponseFrameBytes by construction
	reqPtr := int32(uint32(uintptr(unsafe.Pointer(unsafe.SliceData(frame)))))
	outPtr := int32(uint32(uintptr(unsafe.Pointer(unsafe.SliceData(out)))))
	written := edgerFetch(reqPtr, int32(len(frame)), outPtr, int32(len(out)))
	// The buffers are Go-heap; keep them rooted across the import (the
	// host copied the request before the transport await and writes the
	// response before this call returns).
	runtime.KeepAlive(frame)
	runtime.KeepAlive(out)

	switch {
	case written < 0:
		switch written {
		case -1:
			return nil, errEgressUnavailable
		case -2:
			return nil, errEgressInvalid
		case -3:
			return nil, errEgressResponseLarge
		default: // -4 and anything else stable-negative
			return nil, errEgressTransport
		}
	case written < minResponseFrameBytes || written > maxEgressFrameBytes:
		return nil, errEgressProtocol
	}
	// Decode the response frame with exact bounds: nothing is read past
	// `written`, nothing partial is ever returned.
	n := int(written)
	status := int(binary.LittleEndian.Uint16(out[0:2]))
	if status < 100 || status > 599 {
		return nil, errEgressProtocol
	}
	respHeadersLen := int64(binary.LittleEndian.Uint32(out[4:8]))
	respBodyLen := int64(binary.LittleEndian.Uint32(out[8:12]))
	if respHeadersLen > maxHeaderJSONBytes || respBodyLen > maxResponseBodyBytes {
		return nil, errEgressProtocol
	}
	if responsePrefixBytes+respHeadersLen+respBodyLen != int64(n) {
		return nil, errEgressProtocol
	}
	respHeadersJSON := out[responsePrefixBytes : responsePrefixBytes+respHeadersLen]
	var respPairs [][2]string
	if err := json.Unmarshal(respHeadersJSON, &respPairs); err != nil {
		return nil, errEgressProtocol
	}
	if !validHeaderPairs(respPairs) {
		return nil, errEgressProtocol
	}
	respBody := make([]byte, int(respBodyLen))
	copy(respBody, out[responsePrefixBytes+respHeadersLen:n])
	header := make(http.Header, len(respPairs))
	for _, p := range respPairs {
		header.Add(p[0], p[1])
	}
	return &http.Response{
		Status:        fmt.Sprintf("%d %s", status, http.StatusText(status)),
		StatusCode:    status,
		Proto:         "HTTP/1.1",
		ProtoMajor:    1,
		ProtoMinor:    1,
		Header:        header,
		Body:          io.NopCloser(bytes.NewReader(respBody)),
		Request:       req,
		ContentLength: int64(respBodyLen),
	}, nil
}
