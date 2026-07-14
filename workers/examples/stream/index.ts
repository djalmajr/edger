Deno.serve(() => {
  let timer: ReturnType<typeof setInterval> | undefined;
  const body = new ReadableStream({
    start(controller) {
      controller.enqueue("Hello, World!\n");
      timer = setInterval(() => {
        controller.enqueue("Hello, World!\n");
        console.log("sent");
      }, 1000);
    },
    cancel() {
      console.log("request canceled");
      if (timer !== undefined) {
        clearInterval(timer);
      }
    },
  });
  return new Response(body.pipeThrough(new TextEncoderStream()), {
    headers: {
      "content-type": "text/plain; charset=utf-8",
      "cache-control": "no-cache",
    },
  });
});
