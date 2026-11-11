const fib = n => n < 2 ? n : fib(n - 1) + fib(n - 2);
const users = () => Array.from({ length: 100 }, (_, i) => ({
  id: i, name: `User ${i}`, email: `user${i}@example.com`, active: i % 2 === 0, score: i * 1.5 }));
Bun.serve({ port: 3002, hostname: '127.0.0.1', fetch(req) {
  const u = new URL(req.url);
  if (u.pathname === '/json') return Response.json({ message: 'Hello, World!' });
  if (u.pathname === '/users') return Response.json(users());
  if (u.pathname === '/compute') return Response.json({ result: fib(+(u.searchParams.get('n') ?? 25)) });
  return new Response('Not found', { status: 404 });
}});
