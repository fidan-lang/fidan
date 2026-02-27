// Copyright (c) Kaan Gönüldinc (AppSolves). All rights reserved.
// See LICENSE file in the project root for full license information.

#ifndef FIDAN_PARSER_H
#define FIDAN_PARSER_H

// Include necessary headers
#include <vector>
#include "tokenizer.h"
#include "ast.h"
#include <string>

// Class for the parser
class Parser
{
public:
    // Constructor
    explicit Parser(const std::vector<Token> &tokens, const std::string &filename);
    // Method to parse the tokens and return the AST
    std::vector<std::unique_ptr<ASTNode>> parse(); // Parse the tokens and return the AST

private:
    // The tokens of the program
    const std::vector<Token> &tokens;
    // The name of the file being parsed
    const std::string &filename;
    // The current token index
    size_t currentTokenIndex;
    // The index of the first token of the current statement
    size_t statementFirstTokenIndex = 0;
    // The `ScopeManager` object
    ScopeManager scopeManager;

    // Methods for parsing the tokens
    Token advance();
    bool match(TokenType type, const std::string &value = "", bool advanceIfMatch = true);
    bool peekMatch(TokenType type, int steps = 0, const std::string &value = "");
    void consume(TokenType type, const std::string &value = "", const std::string &errorMessage = "Unexpected token");
    std::unique_ptr<ASTNode> parseStatement();
    std::unique_ptr<ASTNode> parseDecorator();
    std::unique_ptr<ASTNode> parseIfStatement();
    std::unique_ptr<ASTNode> parseTryCatchStatement();
    std::unique_ptr<ASTNode> parseThrowStatement();
    std::unique_ptr<ASTNode> parseReturnStatement();
    std::unique_ptr<ASTNode> parseForLoop();
    std::unique_ptr<ASTNode> parseWhileLoop();
    std::unique_ptr<ASTNode> parseAssignmentOrCall();
    std::unique_ptr<ASTNode> parseVariableDeclaration();
    std::unique_ptr<ASTNode> parseActionDeclaration();
    std::unique_ptr<ASTNode> parseObjectDeclaration();
    std::unique_ptr<ASTNode> parseBlock();
    std::unique_ptr<ASTNode> parseExpression(int precedence = 0);
    std::unique_ptr<ASTNode> parsePrimary();
    std::unique_ptr<ASTNode> parseFunctionCall(const std::vector<std::string_view> &identifierParts);
    std::vector<std::string_view> parseFullIdentifier();
    int getPrecedence(const Token &token);
    void parseParameters(std::vector<Parameter> &parameters, bool canBeOptional = true);
};

#endif // FIDAN_PARSER_H
