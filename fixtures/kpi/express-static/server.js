const http = require("http");
const path = require("path");
const express = require("express");

const app = express();
app.use(express.static(path.join(__dirname, "public")));

const port = Number(process.env.KPI_PORT || process.env.PORT || 0);
const server = http.createServer(app);
server.listen(port, "127.0.0.1", () => {
  const address = server.address();
  const port = address && typeof address === "object" ? address.port : 0;
  console.log(`listening ${port}`);
});
