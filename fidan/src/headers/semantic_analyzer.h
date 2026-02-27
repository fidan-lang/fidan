// Copyright (c) Kaan Gönüldinc (AppSolves). All rights reserved.
// See LICENSE file in the project root for full license information.

#ifndef FIDAN_SEMANTIC_ANALYZER_H
#define FIDAN_SEMANTIC_ANALYZER_H

// Include necessary headers
#include <vector>
#include <memory>
#include "ast.h"
#include "helpers.h"
#include "errors.h"
#include <unordered_set>

// Class for the semantic analyzer
class SemanticAnalyzer
{
public:
    // Constructor
    explicit SemanticAnalyzer(const std::vector<Token> &tokens, const std::vector<std::unique_ptr<ASTNode>> &statements, const std::string &filename);
    // Method to analyze the AST
    void analyze();

private:
    // The tokens of the program
    const std::vector<Token> &tokens;
    // The statements of the program
    const std::vector<std::unique_ptr<ASTNode>> &statements;
    // The name of the file being analyzed
    const std::string &filename;
    // Symbol table to store the ASTNodes
    std::unordered_set<const ASTNode *> symbolTable;
    // Functions to analyze the statements
    void analyzeStatement(const std::unique_ptr<ASTNode> &statement);
    void analyzeLiteral(const Literal *literal);
    void analyzeVariableDeclaration(const VariableDeclaration *declaration);
};

#endif // FIDAN_SEMANTIC_ANALYZER_H