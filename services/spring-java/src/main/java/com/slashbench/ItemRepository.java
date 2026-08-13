package com.slashbench;

import org.springframework.jdbc.core.JdbcTemplate;
import org.springframework.jdbc.core.RowMapper;
import org.springframework.stereotype.Repository;

import java.sql.ResultSet;
import java.time.OffsetDateTime;
import java.util.List;
import java.util.Optional;

@Repository
public class ItemRepository {

    private static final RowMapper<Item> ITEM_MAPPER = (ResultSet rs, int rowNum) -> new Item(
        rs.getLong("id"),
        rs.getString("name"),
        rs.getString("description"),
        rs.getInt("price_cents"),
        rs.getInt("quantity"),
        rs.getObject("created_at", OffsetDateTime.class),
        rs.getObject("updated_at", OffsetDateTime.class)
    );

    private final JdbcTemplate jdbcTemplate;

    public ItemRepository(JdbcTemplate jdbcTemplate) {
        this.jdbcTemplate = jdbcTemplate;
    }

    public Item create(NewItem newItem) {
        return jdbcTemplate.queryForObject(
            """
            INSERT INTO items (name, description, price_cents, quantity)
            VALUES (?, ?, ?, ?)
            RETURNING id, name, description, price_cents, quantity, created_at, updated_at
            """,
            ITEM_MAPPER,
            newItem.name(), newItem.description(), newItem.priceCents(), newItem.quantity()
        );
    }

    public Optional<Item> findById(long id) {
        List<Item> results = jdbcTemplate.query(
            "SELECT id, name, description, price_cents, quantity, created_at, updated_at FROM items WHERE id = ?",
            ITEM_MAPPER,
            id
        );
        return results.stream().findFirst();
    }

    public List<Item> list(long limit, long offset) {
        return jdbcTemplate.query(
            "SELECT id, name, description, price_cents, quantity, created_at, updated_at FROM items ORDER BY id LIMIT ? OFFSET ?",
            ITEM_MAPPER,
            limit, offset
        );
    }

    public long count() {
        Long total = jdbcTemplate.queryForObject("SELECT COUNT(*) FROM items", Long.class);
        return total != null ? total : 0L;
    }
}
