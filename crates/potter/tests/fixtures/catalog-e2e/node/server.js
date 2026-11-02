const http = require("node:http");
const ms = require("ms");

http
  .createServer((_req, res) => res.end(`up for ${ms(Math.round(process.uptime() * 1000))}\n`))
  .listen(Number(process.env.PORT || 3000));
