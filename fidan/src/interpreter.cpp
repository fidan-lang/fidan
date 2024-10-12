// Copyright (c) AppSolves (Kaan Gönüldinc). All rights reserved.
// See LICENSE file in the project root for full license information.

#include "headers/interpreter.h"

InterpreterVM::InterpreterVM(std::vector<std::unique_ptr<ASTNode>> &ast) : ast(ast) {}

void InterpreterVM::interpret()
{
    for (const auto &node : ast)
    {
        evaluateStatement(node.get());
    }
}

void InterpreterVM::evaluateStatement(ASTNode *node)
{
    // TODO: Implement this
}