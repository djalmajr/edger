addEventListener("beforeunload", (event) => {
  console.log(`lifecycle-demo draining: ${event.detail?.reason || "unknown"}`);
  EdgeRuntime.waitUntil(new Promise((resolve) => setTimeout(resolve, 20)));
});

export default function fetch() {
  return Response.json({ ok: true, worker: "lifecycle-demo" });
}
