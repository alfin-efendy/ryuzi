// apps/router/src/observability/otel.ts
// Real OTLP telemetry adapter — active only when `otel_endpoint` is configured.
// Every method body is try/caught so a telemetry failure NEVER throws into the core.

import { SpanStatusCode } from "@opentelemetry/api";
import type { Counter, Histogram } from "@opentelemetry/api";
import { BasicTracerProvider, BatchSpanProcessor } from "@opentelemetry/sdk-trace-base";
import { MeterProvider, PeriodicExportingMetricReader } from "@opentelemetry/sdk-metrics";
import { OTLPTraceExporter } from "@opentelemetry/exporter-trace-otlp-http";
import { OTLPMetricExporter } from "@opentelemetry/exporter-metrics-otlp-http";
import { resourceFromAttributes } from "@opentelemetry/resources";
import { ATTR_SERVICE_NAME } from "@opentelemetry/semantic-conventions";
import type { Telemetry, Span, Attrs } from "./types";

export function createOtelTelemetry(opts: { endpoint: string; serviceName?: string }): Telemetry {
  const resource = resourceFromAttributes({
    [ATTR_SERVICE_NAME]: opts.serviceName ?? "harness-router",
  });

  const tracerProvider = new BasicTracerProvider({
    resource,
    spanProcessors: [new BatchSpanProcessor(new OTLPTraceExporter({ url: `${opts.endpoint}/v1/traces` }))],
  });

  const meterProvider = new MeterProvider({
    resource,
    readers: [
      new PeriodicExportingMetricReader({
        exporter: new OTLPMetricExporter({ url: `${opts.endpoint}/v1/metrics` }),
      }),
    ],
  });

  const tracer = tracerProvider.getTracer("harness-router");
  const meter = meterProvider.getMeter("harness-router");

  // Lazy instrument caches — noUncheckedIndexedAccess: we always check before use via Map.get
  const counters = new Map<string, Counter>();
  const histos = new Map<string, Histogram>();

  return {
    startSpan(name: string, attrs: Attrs = {}): Span {
      try {
        const span = tracer.startSpan(name, { attributes: attrs });
        return {
          setAttribute(k: string, v: string | number | boolean): void {
            try {
              span.setAttribute(k, v);
            } catch {
              /* swallow */
            }
          },
          setError(m: string): void {
            try {
              span.recordException(m);
              span.setStatus({ code: SpanStatusCode.ERROR, message: m });
            } catch {
              /* swallow */
            }
          },
          end(): void {
            try {
              span.end();
            } catch {
              /* swallow */
            }
          },
        };
      } catch {
        return { setAttribute() {}, setError() {}, end() {} };
      }
    },

    count(name: string, attrs: Attrs = {}): void {
      try {
        let c = counters.get(name);
        if (!c) {
          c = meter.createCounter(name);
          counters.set(name, c);
        }
        c.add(1, attrs);
      } catch {
        /* swallow */
      }
    },

    record(name: string, value: number, attrs: Attrs = {}): void {
      try {
        let h = histos.get(name);
        if (!h) {
          h = meter.createHistogram(name);
          histos.set(name, h);
        }
        h.record(value, attrs);
      } catch {
        /* swallow */
      }
    },

    async shutdown() {
      try {
        await tracerProvider.shutdown();
      } catch {}
      try {
        await meterProvider.shutdown();
      } catch {}
    },
  };
}
