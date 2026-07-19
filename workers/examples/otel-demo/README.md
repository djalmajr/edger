# EdgeR OpenTelemetry worker example

This worker extracts the W3C `traceparent` header injected by EdgeR, creates an
`otel-demo.handle` child span, and exports it with OTLP HTTP/protobuf.

The checked-in manifest targets a Collector or Jaeger Service named `jaeger`
in the same Kubernetes namespace. It also permits `registry.npmjs.org:443` so
Deno can populate a cold `npm:` cache. For another environment, change
`OTEL_EXPORTER_OTLP_TRACES_ENDPOINT` and the collector entry in `allowNet`
before packaging the worker.

With the EdgeR runtime and OTLP backend running, call:

```bash
curl http://localhost:3000/otel-demo/
```

The response includes the exported trace ID. In Jaeger, the same trace contains
the EdgeR `worker.dispatch` and `pool.fetch_stream` spans followed by the worker
service span `otel-demo.handle`.
