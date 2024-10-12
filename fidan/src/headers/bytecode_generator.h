// Copyright (c) AppSolves (Kaan Gönüldinc). All rights reserved.
// See LICENSE file in the project root for full license information.

#ifndef FIDAN_BYTECODE_GENERATOR_H
#define FIDAN_BYTECODE_GENERATOR_H

#include <stdint.h>
#include <stdlib.h>

typedef enum
{
    OP_RETURN,
} OpCode;

typedef struct
{
    int count;
    int capacity;
    uint8_t *code;
} Chunk;

#define GROW_CAPACITY(capacity) \
    ((capacity) < 8 ? 8 : (capacity) * 2)

#define GROW_ARRAY(type, pointer, oldCount, newCount)      \
    (type *)reallocate(pointer, sizeof(type) * (oldCount), \
                       sizeof(type) * (newCount))

#define FREE_ARRAY(type, pointer, oldCount) \
    reallocate(pointer, sizeof(type) * (oldCount), 0)

void *reallocate(void *pointer, size_t oldSize, size_t newSize);

void initChunk(Chunk *chunk);
void freeChunk(Chunk *chunk);
void writeChunk(Chunk *chunk, uint8_t byte);

#endif // FIDAN_BYTECODE_GENERATOR_H