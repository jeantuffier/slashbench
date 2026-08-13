package com.slashbench;

import org.springframework.http.HttpStatus;
import org.springframework.http.ResponseEntity;
import org.springframework.web.bind.annotation.GetMapping;
import org.springframework.web.bind.annotation.PathVariable;
import org.springframework.web.bind.annotation.PostMapping;
import org.springframework.web.bind.annotation.RequestBody;
import org.springframework.web.bind.annotation.RequestParam;
import org.springframework.web.bind.annotation.RestController;

@RestController
public class ItemController {

    private final ItemRepository repository;

    public ItemController(ItemRepository repository) {
        this.repository = repository;
    }

    @PostMapping("/items")
    public ResponseEntity<Item> create(@RequestBody NewItem newItem) {
        Item created = repository.create(newItem);
        return ResponseEntity.status(HttpStatus.CREATED).body(created);
    }

    @GetMapping("/items/{id}")
    public ResponseEntity<?> getById(@PathVariable long id) {
        return repository.findById(id)
            .<ResponseEntity<?>>map(ResponseEntity::ok)
            .orElseGet(() -> ResponseEntity.status(HttpStatus.NOT_FOUND).body(new ErrorBody("not found")));
    }

    @GetMapping("/items")
    public ItemList list(
        @RequestParam(defaultValue = "1") long page,
        @RequestParam(defaultValue = "20") long limit
    ) {
        long safePage = Math.max(page, 1);
        long safeLimit = Math.min(Math.max(limit, 1), 100);
        long offset = (safePage - 1) * safeLimit;

        var items = repository.list(safeLimit, offset);
        long total = repository.count();
        return new ItemList(items, safePage, safeLimit, total);
    }
}
