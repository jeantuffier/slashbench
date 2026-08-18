import { serve } from '@hono/node-server'
import { Hono } from 'hono'
import http from 'node:http'
import sql from './db.js'

const app = new Hono()

app.post('/items', async (c) => {
  const body = await c.req.json()
  const [item] = await sql`
    INSERT INTO items (name, description, price_cents, quantity)
    VALUES (${body.name}, ${body.description ?? null}, ${body.price_cents}, ${body.quantity})
    RETURNING id, name, description, price_cents, quantity, created_at, updated_at
  `
  return c.json(item, 201)
})

app.get('/items/:id', async (c) => {
  const id = Number(c.req.param('id'))
  const [item] = await sql`
    SELECT id, name, description, price_cents, quantity, created_at, updated_at
    FROM items WHERE id = ${id}
  `
  if (!item) {
    return c.json({ error: 'not found' }, 404)
  }
  return c.json(item)
})

app.get('/items', async (c) => {
  const page = Math.max(Number(c.req.query('page') ?? '1'), 1)
  const limit = Math.min(Math.max(Number(c.req.query('limit') ?? '20'), 1), 100)
  const offset = (page - 1) * limit

  const items = await sql`
    SELECT id, name, description, price_cents, quantity, created_at, updated_at
    FROM items ORDER BY id LIMIT ${limit} OFFSET ${offset}
  `
  const [{ count }] = await sql`SELECT COUNT(*) FROM items`

  return c.json({ items, page, limit, total: count })
})

app.onError((err, c) => c.json({ error: err.message }, 500))

serve({
  fetch: app.fetch,
  port: 8080,
  hostname: '0.0.0.0',
  createServer: (options, listener) => {
    const server = http.createServer(options, listener)
    server.keepAliveTimeout = 75_000
    const realListen = server.listen.bind(server)
    server.listen = (port, hostname, callback) => realListen(port, hostname, 4096, callback)
    return server
  },
})
