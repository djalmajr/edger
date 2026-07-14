interface reqPayload {
  name: string;
}

console.info("server started modified");

Deno.serve(async (req: Request) => {
  let name = "World";
  if (req.body) {
    const payload = await req.json().catch(() => null) as reqPayload | null;
    if (payload && typeof payload.name === "string" && payload.name.trim()) {
      name = payload.name.trim();
    }
  }
  const data = {
    message: `Hello ${name} from foo!`,
  };

  return new Response(
    JSON.stringify(data),
    {
      headers: {
        "Content-Type": "application/json",
      },
    },
  );
});
