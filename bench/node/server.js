const http = require('node:http');
const fib = n => n < 2 ? n : fib(n - 1) + fib(n - 2);
const users = () => Array.from({ length: 100 }, (_, i) => ({
  id: i, name: `User ${i}`, email: `user${i}@example.com`, active: i % 2 === 0, score: i * 1.5 }));
const send = (res, obj) => { const b = JSON.stringify(obj);
  res.writeHead(200, { 'Content-Type': 'application/json', 'Content-Length': Buffer.byteLength(b) }); res.end(b); };
http.createServer((req, res) => {
  const u = new URL(req.url, 'http://x');
  if (u.pathname === '/json') return send(res, { message: 'Hello, World!' });
  if (u.pathname === '/users') return send(res, users());
  if (u.pathname === '/compute') return send(res, { result: fib(+(u.searchParams.get('n') ?? 25)) });
  res.writeHead(404); res.end();
}).listen(3001, '127.0.0.1');
