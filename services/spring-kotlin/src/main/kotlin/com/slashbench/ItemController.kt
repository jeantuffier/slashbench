package com.slashbench

import org.springframework.http.HttpStatus
import org.springframework.http.ResponseEntity
import org.springframework.web.bind.annotation.GetMapping
import org.springframework.web.bind.annotation.PathVariable
import org.springframework.web.bind.annotation.PostMapping
import org.springframework.web.bind.annotation.RequestBody
import org.springframework.web.bind.annotation.RequestParam
import org.springframework.web.bind.annotation.RestController

@RestController
class ItemController(private val repository: ItemRepository) {

    @PostMapping("/items")
    fun create(@RequestBody newItem: NewItem): ResponseEntity<Item> {
        val created = repository.create(newItem)
        return ResponseEntity.status(HttpStatus.CREATED).body(created)
    }

    @GetMapping("/items/{id}")
    fun getById(@PathVariable id: Long): ResponseEntity<Any> {
        val item = repository.findById(id)
        return if (item != null) {
            ResponseEntity.ok(item)
        } else {
            ResponseEntity.status(HttpStatus.NOT_FOUND).body(ErrorBody("not found"))
        }
    }

    @GetMapping("/items")
    fun list(
        @RequestParam(defaultValue = "1") page: Long,
        @RequestParam(defaultValue = "20") limit: Long
    ): ItemList {
        val safePage = page.coerceAtLeast(1)
        val safeLimit = limit.coerceIn(1, 100)
        val offset = (safePage - 1) * safeLimit

        val items = repository.list(safeLimit, offset)
        val total = repository.count()
        return ItemList(items, safePage, safeLimit, total)
    }
}
