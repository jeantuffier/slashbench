package com.slashbench;

import java.time.OffsetDateTime;

public record Item(
    long id,
    String name,
    String description,
    int priceCents,
    int quantity,
    OffsetDateTime createdAt,
    OffsetDateTime updatedAt
) {}
