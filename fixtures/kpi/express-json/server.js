const http = require("http");
const express = require("express");

const app = express();
app.use(express.json());

app.post("/echo", (req, res) => {
  const body = req.body && typeof req.body === "object" ? req.body : {};
  res.status(200).json({ ok: true, body });
});

const port = Number(process.env.KPI_PORT || process.env.PORT || 0);
const server = http.createServer(app);
server.listen(port, "127.0.0.1", () => {
  const address = server.address();
  const port = address && typeof address === "object" ? address.port : 0;
  console.log(`listening ${port}`);
});
