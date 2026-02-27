// Copyright (c) Kaan Gönüldinc (AppSolves). All rights reserved.
// See LICENSE file in the project root for full license information.

#ifndef FIDAN_INTERPRETER_H
#define FIDAN_INTERPRETER_H

// Include necessary headers
#include <vector>
#include "../headers/ast.h"

// Class for the interpreter virtual machine
class InterpreterVM
{
public:
    // Constructor
    explicit InterpreterVM(std::vector<std::unique_ptr<ASTNode>> &ast);
    // Method to interpret the AST
    void interpret();

private:
    // AST of the program
    std::vector<std::unique_ptr<ASTNode>> &ast;
    // Method to evaluate a statement
    void evaluateStatement(ASTNode *node);
    // More methods for evaluating expressions, variables, etc.
};

#endif // FIDAN_INTERPRETER_H
