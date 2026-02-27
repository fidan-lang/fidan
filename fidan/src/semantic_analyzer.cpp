// Copyright (c) Kaan Gönüldinc (AppSolves). All rights reserved.
// See LICENSE file in the project root for full license information.

#include "headers/semantic_analyzer.h"
#include <iostream>

// Constructor implementation
SemanticAnalyzer::SemanticAnalyzer(const std::vector<Token> &tokens, const std::vector<std::unique_ptr<ASTNode>> &statements, const std::string &filename)
    : tokens(tokens), statements(statements), filename(filename)
{
    // Reserve space in the symbol table
    symbolTable.reserve(statements.size());
}

// Implement the analyze method
void SemanticAnalyzer::analyze()
{
    if (isDebugMode)
        print("SEMANTIC ANALYSIS: ", 2);
    for (const auto &statement : statements)
    {
        analyzeStatement(statement);
    }
}

// Implement the analyzeStatement method
void SemanticAnalyzer::analyzeStatement(const std::unique_ptr<ASTNode> &statement)
{
    RuntimeError unknownStatementType("Unknown statement type", tokens[statement->firstTokenIndex].line, tokens[statement->firstTokenIndex].column);
    TraceGuard guard(unknownStatementType, filename, "SemanticAnalyzer::analyzeStatement", tokens[statement->firstTokenIndex].line);

    if (isDebugMode)
    {
        // Print the indentifiers that are inside the symbolTable
        for (const auto &entry : symbolTable)
        {
            print("SymbolTable: " + entry->toString());
        }
    }

    switch (statement->getNodeType())
    {
    case NodeType::Literal:
    {
        Literal *literal = dynamic_cast<Literal *>(statement.get());
        analyzeLiteral(literal);
        break;
    }
    case NodeType::Block:
    {
        Block *block = dynamic_cast<Block *>(statement.get());
        for (const auto &stmt : block->statements)
        {
            analyzeStatement(stmt);
        }
        break;
    }
    case NodeType::VariableDeclaration:
    {
        VariableDeclaration *declaration = dynamic_cast<VariableDeclaration *>(statement.get());
        analyzeVariableDeclaration(declaration);
        break;
    }
    default:
        throw unknownStatementType;
    }
}

// Implement the analyzeLiteral method
void SemanticAnalyzer::analyzeLiteral(const Literal *literal)
{
    TypeError literalTypeError("Literal type cannot be 'dynamic'", tokens[literal->firstTokenIndex].line, tokens[literal->firstTokenIndex].column);

    if (literal->type == "dynamic")
    {
        TraceGuard guard(literalTypeError, filename, "SemanticAnalyzer::analyzeLiteral", tokens[literal->firstTokenIndex].line);
        throw literalTypeError;
    }
}

// Implement the analyzeVariableDeclaration method
void SemanticAnalyzer::analyzeVariableDeclaration(const VariableDeclaration *declaration)
{
    LogicError variableRedeclarationError("Variable '" + join(declaration->identifierParts, ".") + "' cannot be redeclared after a value has been assigned", tokens[declaration->firstTokenIndex].line, tokens[declaration->firstTokenIndex].column);
    TypeError variableTypeError("Type mismatch in variable declaration", tokens[declaration->firstTokenIndex].line, tokens[declaration->firstTokenIndex].column);

    // Check if the variable has already been declared and has a value
    // Check if there is an ASTNode with the same identifier in the symbol table
    if (symbolTable.find(declaration) != symbolTable.end() && declaration->initializer != nullptr)
    {
        TraceGuard guard(variableRedeclarationError, filename, "SemanticAnalyzer::analyzeVariableDeclaration", tokens[declaration->firstTokenIndex].line);
        throw variableRedeclarationError;
    }
    if (declaration->initializer != nullptr)
    {
        analyzeStatement(declaration->initializer);
    }
    if (declaration->type != "dynamic" && declaration->initializer != nullptr)
    {
        // Check if the type of the initializer matches the type of the variable
        if (declaration->initializer->getNodeType() != NodeType::Literal || dynamic_cast<Literal *>(declaration->initializer.get())->type != declaration->type)
        {
            TraceGuard guard(variableTypeError, filename, "SemanticAnalyzer::analyzeVariableDeclaration", tokens[declaration->firstTokenIndex].line);
            throw variableTypeError;
        }
    }

    // Since Fidan is a flexible and permissive language, a varible can be declared multiple times, overriding the previous declaration
    symbolTable.insert(declaration);
}