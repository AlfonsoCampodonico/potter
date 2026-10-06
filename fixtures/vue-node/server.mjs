import { createReadStream } from 'node:fs'
import { readFile, stat } from 'node:fs/promises'
import { createServer } from 'node:http'
import { extname, join, normalize } from 'node:path'
import { fileURLToPath } from 'node:url'

const root = fileURLToPath(new URL('./dist/', import.meta.url))
const { version } = JSON.parse(
  await readFile(new URL('./package.json', import.meta.url), 'utf8'),
)

const port = Number(process.env.PORT ?? 8080)
// Platforms may route over IPv6, so bind dual-stack rather than 0.0.0.0.
const host = process.env.HOST ?? '::'

const mimeTypes = {
  '.css': 'text/css; charset=utf-8',
  '.html': 'text/html; charset=utf-8',
  '.ico': 'image/x-icon',
  '.js': 'text/javascript; charset=utf-8',
  '.json': 'application/json; charset=utf-8',
  '.map': 'application/json; charset=utf-8',
  '.svg': 'image/svg+xml',
  '.woff2': 'font/woff2',
}

function json(response, status, body) {
  const payload = JSON.stringify(body)
  response.writeHead(status, {
    'content-type': 'application/json; charset=utf-8',
    'content-length': Buffer.byteLength(payload),
  })
  response.end(payload)
}

async function sendFile(response, path, status = 200) {
  const info = await stat(path)
  response.writeHead(status, {
    'content-type': mimeTypes[extname(path)] ?? 'application/octet-stream',
    'content-length': info.size,
  })
  createReadStream(path).pipe(response)
}

export const server = createServer(async (request, response) => {
  const url = new URL(request.url, `http://${request.headers.host ?? 'localhost'}`)

  if (request.method !== 'GET' && request.method !== 'HEAD') {
    return json(response, 405, { error: 'method not allowed' })
  }

  if (url.pathname === '/health') {
    return json(response, 200, { status: 'ok', version, node: process.versions.node })
  }

  const greeting = url.pathname.match(/^\/api\/greetings\/([^/]+)$/)
  if (greeting) {
    const name = decodeURIComponent(greeting[1]).slice(0, 64)
    return json(response, 200, { message: `Hello, ${name}! 👋` })
  }

  if (url.pathname.startsWith('/api/')) {
    return json(response, 404, { error: 'not found' })
  }

  // Static assets, falling back to the SPA entry point for unknown routes.
  const relative = normalize(decodeURIComponent(url.pathname)).replace(/^(\.\.[/\\])+/, '')
  try {
    await sendFile(response, join(root, relative === '/' ? 'index.html' : relative))
  } catch {
    try {
      await sendFile(response, join(root, 'index.html'))
    } catch {
      json(response, 404, { error: 'not found' })
    }
  }
})

if (process.env.NODE_ENV !== 'test') {
  server.listen(port, host, () => {
    console.log(`vue-hello-world listening on [${host}]:${port}`)
  })
}
