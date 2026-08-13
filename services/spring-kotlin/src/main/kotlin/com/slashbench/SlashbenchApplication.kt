package com.slashbench

import org.springframework.boot.autoconfigure.SpringBootApplication
import org.springframework.boot.runApplication

@SpringBootApplication
class SlashbenchApplication

fun main(args: Array<String>) {
    runApplication<SlashbenchApplication>(*args)
}
