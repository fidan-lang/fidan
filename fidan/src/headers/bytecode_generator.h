// Copyright (c) Kaan Gönüldinc (AppSolves). All rights reserved.
// See LICENSE file in the project root for full license information.

#ifndef FIDAN_BYTECODE_GENERATOR_H
#define FIDAN_BYTECODE_GENERATOR_H

// Include necessary headers
#include <stdint.h>
#include <stdlib.h>

// Enum for the opcodes
typedef enum
{
    OP_RETURN,
} OpCode;

// Struct for the chunk
typedef struct
{
    int count;
    int capacity;
    uint8_t *code;
} Chunk;

// Macros for the memory management
#define GROW_CAPACITY(capacity) \
    ((capacity) < 8 ? 8 : (capacity) * 2)

#define GROW_ARRAY(type, pointer, oldCount, newCount)      \
    (type *)reallocate(pointer, sizeof(type) * (oldCount), \
                       sizeof(type) * (newCount))

#define FREE_ARRAY(type, pointer, oldCount) \
    reallocate(pointer, sizeof(type) * (oldCount), 0)

// Function declarations to manage the memory
void *reallocate(void *pointer, size_t oldSize, size_t newSize);

void initChunk(Chunk *chunk);
void freeChunk(Chunk *chunk);
void writeChunk(Chunk *chunk, uint8_t byte);

#endif // FIDAN_BYTECODE_GENERATOR_H