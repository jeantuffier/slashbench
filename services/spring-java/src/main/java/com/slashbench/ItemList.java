package com.slashbench;

import java.util.List;

public record ItemList(
    List<Item> items,
    long page,
    long limit,
    long total
) {}
