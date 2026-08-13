import postgres from 'postgres'

// postgres.js returns int8 (bigint) columns as strings by default, to avoid
// silent precision loss beyond Number.MAX_SAFE_INTEGER. This benchmark's ids
// and counts never get close to that range, and returning them as JSON
// strings instead of numbers would break the shared contract with the
// Rust/JVM stacks — so override the int8 parser to hand back a plain Number.
const sql = postgres(process.env.DATABASE_URL, {
  types: {
    bigint: {
      to: 20,
      from: [20],
      serialize: (x) => String(x),
      parse: (x) => parseInt(x, 10),
    },
  },
})

export default sql
