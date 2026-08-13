package com.slashbench

import org.springframework.jdbc.core.JdbcTemplate
import org.springframework.jdbc.core.RowMapper
import org.springframework.stereotype.Repository
import java.sql.ResultSet
import java.time.OffsetDateTime

@Repository
class ItemRepository(private val jdbcTemplate: JdbcTemplate) {

    private val itemMapper = RowMapper { rs: ResultSet, _: Int ->
        Item(
            id = rs.getLong("id"),
            name = rs.getString("name"),
            description = rs.getString("description"),
            priceCents = rs.getInt("price_cents"),
            quantity = rs.getInt("quantity"),
            createdAt = rs.getObject("created_at", OffsetDateTime::class.java),
            updatedAt = rs.getObject("updated_at", OffsetDateTime::class.java)
        )
    }

    fun create(newItem: NewItem): Item {
        return jdbcTemplate.queryForObject(
            """
            INSERT INTO items (name, description, price_cents, quantity)
            VALUES (?, ?, ?, ?)
            RETURNING id, name, description, price_cents, quantity, created_at, updated_at
            """.trimIndent(),
            itemMapper,
            newItem.name, newItem.description, newItem.priceCents, newItem.quantity
        )!!
    }

    fun findById(id: Long): Item? {
        val results = jdbcTemplate.query(
            "SELECT id, name, description, price_cents, quantity, created_at, updated_at FROM items WHERE id = ?",
            itemMapper,
            id
        )
        return results.firstOrNull()
    }

    fun list(limit: Long, offset: Long): List<Item> {
        return jdbcTemplate.query(
            "SELECT id, name, description, price_cents, quantity, created_at, updated_at FROM items ORDER BY id LIMIT ? OFFSET ?",
            itemMapper,
            limit, offset
        )
    }

    fun count(): Long {
        return jdbcTemplate.queryForObject("SELECT COUNT(*) FROM items", Long::class.java) ?: 0L
    }
}
