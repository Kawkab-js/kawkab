'use strict';
var http = require('http');
var port = Number(process.env.KPI_PORT || process.env.PORT || 0);
var server = http.createServer(function (req, res) {
  if (req.url === '/' || req.url.indexOf('/?') === 0) {
    res.writeHead(200, { 'Content-Type': 'text/plain' });
    res.end('kawkab_http_ok');
    return;
  }
  res.writeHead(404);
  res.end();
});
server.listen(port, '127.0.0.1', function () {
  var addr = server.address();
  if (!addr || typeof addr.port !== 'number') {
    console.error('no_address');
    process.exit(2);
  }
  console.log('listening', addr.port);
});
