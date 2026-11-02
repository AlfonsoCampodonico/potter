import ms from "ms";

Bun.serve({
  port: Number(process.env.PORT ?? 3000),
  fetch: () => new Response(`up for ${ms(Math.round(process.uptime() * 1000))}\n`),
});
