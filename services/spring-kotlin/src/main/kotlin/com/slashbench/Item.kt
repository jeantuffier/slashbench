package com.slashbench

import java.time.OffsetDateTime

data class Item(
    val id: Long,
    val name: String,
    val description: String?,
    val priceCents: Int,
    val quantity: Int,
    val createdAt: OffsetDateTime,
    val updatedAt: OffsetDateTime
)

data class NewItem(
    val name: String,
    val description: String?,
    val priceCents: Int,
    val quantity: Int
)

data class ItemList(
    val items: List<Item>,
    val page: Long,
    val limit: Long,
    val total: Long
)

data class ErrorBody(val error: String)
