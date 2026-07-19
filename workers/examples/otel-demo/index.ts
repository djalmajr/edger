import {
  context,
  propagation,
  SpanStatusCode,
  trace,
} from "npm:@opentelemetry/api@1.9.1";
import { W3CTraceContextPropagator } from "npm:@opentelemetry/core@2.9.0";
import { OTLPTraceExporter } from "npm:@opentelemetry/exporter-trace-otlp-proto@0.220.0";
import { resourceFromAttributes } from "npm:@opentelemetry/resources@2.9.0";
import {
  BasicTracerProvider,
  SimpleSpanProcessor,
} from "npm:@opentelemetry/sdk-trace-base@2.9.0";
import {
  ATTR_SERVICE_NAME,
  ATTR_SERVICE_VERSION,
} from "npm:@opentelemetry/semantic-conventions@1.43.0";

const serviceName = Deno.env.get("OTEL_SERVICE_NAME") ?? "otel-demo";
const serviceVersion = "1.0.0";
const tracesEndpoint = Deno.env.get("OTEL_EXPORTER_OTLP_TRACES_ENDPOINT") ??
  "http://localhost:4318/v1/traces";

const provider = new BasicTracerProvider({
  resource: resourceFromAttributes({
    [ATTR_SERVICE_NAME]: serviceName,
    [ATTR_SERVICE_VERSION]: serviceVersion,
  }),
  spanProcessors: [
    new SimpleSpanProcessor(new OTLPTraceExporter({ url: tracesEndpoint })),
  ],
});

trace.setGlobalTracerProvider(provider);
propagation.setGlobalPropagator(new W3CTraceContextPropagator());

const tracer = trace.getTracer("edger-otel-demo", serviceVersion);
const headerGetter = {
  get(carrier: Headers, key: string): string | undefined {
    return carrier.get(key) ?? undefined;
  },
  keys(carrier: Headers): string[] {
    return [...carrier.keys()];
  },
};

Deno.addSignalListener("SIGTERM", () => {
  provider.shutdown().catch((error) => {
    console.error("failed to shut down OpenTelemetry", error);
  });
});

Deno.serve(async (request) => {
  const parentContext = propagation.extract(
    context.active(),
    request.headers,
    headerGetter,
  );
  const span = tracer.startSpan("otel-demo.handle", {
    attributes: {
      "http.request.method": request.method,
      "url.path": new URL(request.url).pathname,
      "edger.worker.name": "otel-demo",
    },
  }, parentContext);

  try {
    await new Promise((resolve) => setTimeout(resolve, 25));
    span.addEvent("example.work.completed");
    span.setStatus({ code: SpanStatusCode.OK });

    return Response.json({
      message: "OpenTelemetry span exported",
      service: serviceName,
      traceId: span.spanContext().traceId,
    });
  } catch (error) {
    span.recordException(error as Error);
    span.setStatus({
      code: SpanStatusCode.ERROR,
      message: error instanceof Error ? error.message : "unknown error",
    });
    throw error;
  } finally {
    span.end();
    await provider.forceFlush();
  }
});
