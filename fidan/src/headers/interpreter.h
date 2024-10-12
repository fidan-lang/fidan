// Copyright (c) AppSolves (Kaan Gönüldinc). All rights reserved.
// See LICENSE file in the project root for full license information.

#ifndef FIDAN_INTERPRETER_H
#define FIDAN_INTERPRETER_H

#include <vector>
#include "../headers/ast.h"

class InterpreterVM
{
public:
    explicit InterpreterVM(std::vector<std::unique_ptr<ASTNode>> &ast);
    void interpret();

private:
    std::vector<std::unique_ptr<ASTNode>> &ast;
    void evaluateStatement(ASTNode *node);
    // More methods for evaluating expressions, variables, etc.
};

#endif // FIDAN_INTERPRETER_H
