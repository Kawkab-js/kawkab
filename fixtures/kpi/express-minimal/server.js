'use strict';
var express = require('express');
var http = require('http');
var app = express();
app.get('/', function (req, res) {
  res.send('kawkab_express_ok');
});
var port = Number(process.env.KPI_PORT || process.env.PORT || 0);
var server = http.createServer(app);
server.listen(port, '127.0.0.1', function () {
  var addr = server.address();
  if (!addr || typeof addr.port !== 'number') {
    console.error('no_address');
    process.exit(2);
  }
  console.log('listening', addr.port);
});
