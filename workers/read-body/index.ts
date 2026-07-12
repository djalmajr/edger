interface reqPayload {
  name: string;
}

console.info("server started modified");

Deno.serve(async (req: Request) => {
  console.log("serving request");
  let totalSize = 0;
  if (req.body) {
    const reader = req.body.getReader();
    while (true) {
      const { done, value } = await reader.read();
      if (done) {
        break;
      }
      totalSize += value.length;
    }
  }

  return new Response(
    JSON.stringify({ totalSize }),
    {
      headers: {
        "Content-Type": "application/json",
      },
    },
  );
});
