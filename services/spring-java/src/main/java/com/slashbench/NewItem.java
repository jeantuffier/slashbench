package com.slashbench;

public record NewItem(
    String name,
    String description,
    int priceCents,
    int quantity
) {}
