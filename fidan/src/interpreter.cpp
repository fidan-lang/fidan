// Copyright (c) Kaan Gönüldinc (AppSolves). All rights reserved.
// See LICENSE file in the project root for full license information.

// Include necessary headers
#include "headers/interpreter.h"

// Constructor
InterpreterVM::InterpreterVM(std::vector<std::unique_ptr<ASTNode>> &ast) : ast(ast) {}

// Method to interpret the AST
void InterpreterVM::interpret()
{
    for (const auto &node : ast)
    {
        evaluateStatement(node.get());
    }
}

// Method to evaluate a statement
void InterpreterVM::evaluateStatement(ASTNode *node)
{
    // TODO: Implement this
}