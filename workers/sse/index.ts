const msg = new TextEncoder().encode("data: hella\r\n\r\n");

Deno.serve(async () => {
  let timerId: ReturnType<typeof setInterval> | undefined;

  const body = new ReadableStream({
    start(controller) {
      controller.enqueue(msg);
      timerId = setInterval(() => {
        controller.enqueue(msg);
      }, 1000);
    },
    cancel() {
      if (timerId !== undefined) {
        clearInterval(timerId);
      }
    },
  });

  return new Response(body, {
    headers: {
      "Content-Type": "text/event-stream",
      "Cache-Control": "no-cache",
    },
  });
});
