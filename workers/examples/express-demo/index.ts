import express from "npm:express@5";

const app = express();
app.get("/", (_req, res) => res.json({ framework: "express" }));
app.get("/users/:id", (req, res) => res.json({ user: req.params.id }));
app.listen(3000);
