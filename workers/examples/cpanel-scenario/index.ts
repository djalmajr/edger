export default async function fetch(request) {
  const url = new URL(request.url);
  const delay = Number(url.searchParams.get("delay") || "0");
  if (delay > 0) await new Promise((resolve) => setTimeout(resolve, delay));
  if (url.pathname.endsWith("/fail")) throw new Error("cpanel scenario failure");
  return Response.json({ version: "1.0.0", delay });
}
