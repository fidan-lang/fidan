// Copyright (c) AppSolves (Kaan Gönüldinc). All rights reserved.
// See LICENSE file in the project root for full license information.

#include "headers/errors.h"
#include "headers/helpers.h"
#include <unordered_set>

Parser::Parser(const std::vector<Token> &tokens, const std::string &filename) : tokens(tokens), filename(filename), currentTokenIndex(0), scopeManager() {}

inline Token Parser::advance()
{
    return currentTokenIndex < tokens.size() ? tokens[currentTokenIndex++] : Token{TokenType::EOF_, "", -1, -1};
}

inline bool Parser::match(TokenType type, const std::string &value, bool advanceIfMatch)
{
    if (currentTokenIndex < tokens.size() && tokens[currentTokenIndex].type == type &&
        (value.empty() || upper(tokens[currentTokenIndex].value) == upper(value)))
    {
        if (advanceIfMatch)
            advance();
        return true;
    }
    return false;
}

inline bool Parser::peekMatch(TokenType type, int steps, const std::string &value)
{
    return currentTokenIndex + steps < tokens.size() && tokens[currentTokenIndex + steps].type == type &&
           (value.empty() || upper(tokens[currentTokenIndex + steps].value) == upper(value));
}

inline void Parser::consume(TokenType type, const std::string &value, const std::string &errorMessage)
{
    if (!match(type, value))
    {
        throw SyntaxError(errorMessage, tokens[currentTokenIndex - 1].line, tokens[currentTokenIndex - 1].column);
    }
}

std::vector<std::unique_ptr<ASTNode>> Parser::parse()
{
    std::optional<BracketError> bracketError = checkBalancedBrackets(tokens); // Check for unbalanced brackets
    if (bracketError.has_value())
    {
        std::string bracketType = bracketError->token.type == TokenType::OPEN_PAREN ? "parentheses" : bracketError->token.type == TokenType::OPEN_BRACE ? "braces"
                                                                                                                                                        : "brackets";
        std::string message = bracketError->isOpening ? "Unmatched opening " + bracketType : "Unmatched closing " + bracketType;
        SyntaxError unbalancedBracketError(message, bracketError->token.line, bracketError->token.column);
        TraceGuard guard(unbalancedBracketError, filename, "Parser::parse", bracketError->token.line);
        throw unbalancedBracketError;
    }
    scopeManager.enterScope("global", ScopeType::Global); // Enter the global scope
    std::vector<std::unique_ptr<ASTNode>> statements;

    while (currentTokenIndex < tokens.size() && tokens[currentTokenIndex].type != TokenType::EOF_)
    {
        statements.push_back(parseStatement());
    }

    scopeManager.exitScope(); // Exit the global scope
    return statements;        // Return the list of statements
}

std::unique_ptr<ASTNode> Parser::parseStatement()
{
    SyntaxError unexpectedTokenError("Unexpected token '" + lower(tokens[currentTokenIndex].value) + "'", tokens[currentTokenIndex].line, tokens[currentTokenIndex].column);
    TraceGuard guard(unexpectedTokenError, filename, "Parser::parseStatement", tokens[currentTokenIndex].line);

    if (match(TokenType::DECORATOR))
    {
        return parseDecorator();
    }
    if (match(TokenType::KEYWORD, "var"))
    {
        return parseVariableDeclaration();
    }
    if (match(TokenType::KEYWORD, "when"))
    {
        return parseIfStatement();
    }
    if (match(TokenType::KEYWORD, "attempt"))
    {
        return parseTryCatchStatement();
    }
    if (match(TokenType::KEYWORD, "throw"))
    {
        return parseThrowStatement();
    }
    if (match(TokenType::KEYWORD, "return"))
    {
        return parseReturnStatement();
    }
    if (match(TokenType::KEYWORD, "action"))
    {
        return parseActionDeclaration();
    }
    if (match(TokenType::KEYWORD, "object"))
    {
        return parseObjectDeclaration();
    }
    if (match(TokenType::KEYWORD, "for"))
    {
        return parseForLoop();
    }
    if (match(TokenType::KEYWORD, "while"))
    {
        return parseWhileLoop();
    }
    if (match(TokenType::OPEN_BRACE))
    {
        return parseBlock();
    }
    if (match(TokenType::KEYWORD, "outer", false) ||
        match(TokenType::KEYWORD, "global", false) ||
        match(TokenType::KEYWORD, "this", false) ||
        match(TokenType::KEYWORD, "parent", false) ||
        match(TokenType::KEYWORD, "self", false) ||
        match(TokenType::KEYWORD, "super", false) ||
        match(TokenType::IDENTIFIER, "", false))
    {
        return parseAssignmentOrCall();
    }

    throw unexpectedTokenError;
}

// Function to parse an decorator
std::unique_ptr<ASTNode> Parser::parseDecorator()
{
    std::string_view name = tokens[currentTokenIndex - 1].value;

    // Decorator without arguments
    scopeManager.enterScope(name, ScopeType::DecoratorPaired); // Enter a new scope for the decorator
    std::unique_ptr<ASTNode> statement = parseStatement();
    scopeManager.exitScope(); // Exit the decorator scope
    return std::make_unique<Decorator>(name, std::move(statement), scopeManager.currentScope());
}

std::unique_ptr<ASTNode> Parser::parseIfStatement()
{
    std::vector<std::pair<std::unique_ptr<ASTNode>, std::unique_ptr<ASTNode>>> conditionsAndBlocks;
    bool openParen = match(TokenType::OPEN_PAREN);
    std::unique_ptr<ASTNode> condition = parseExpression();
    if (openParen)
        consume(TokenType::CLOSE_PAREN, "", "Expected ')' after 'if/when' condition");
    consume(TokenType::OPEN_BRACE, "", "Expected '{' after 'if/when' condition");
    std::unique_ptr<ASTNode> body = parseBlock();
    conditionsAndBlocks.emplace_back(std::move(condition), std::move(body));

    // Check for 'else if' or 'otherwise when' statements
    while (match(TokenType::KEYWORD, "otherwise"))
    {
        if (match(TokenType::KEYWORD, "when"))
        {
            openParen = match(TokenType::OPEN_PAREN);
            std::unique_ptr<ASTNode> elseCondition = parseExpression();
            if (openParen)
                consume(TokenType::CLOSE_PAREN, "", "Expected ')' after 'if/when' condition");
            consume(TokenType::OPEN_BRACE, "", "Expected '{' after 'if/when' condition");
            std::unique_ptr<ASTNode> elseBody = parseBlock();
            conditionsAndBlocks.emplace_back(std::move(elseCondition), std::move(elseBody));
        }
        else
        {
            consume(TokenType::OPEN_BRACE, "", "Expected '{' after 'else/otherwise'");
            std::unique_ptr<ASTNode> elseBody = parseBlock();
            conditionsAndBlocks.emplace_back(nullptr, std::move(elseBody));
            break;
        }
    }

    return std::make_unique<WhenStatement>(std::move(conditionsAndBlocks), scopeManager.currentScope());
}

std::unique_ptr<ASTNode> Parser::parseTryCatchStatement()
{
    SyntaxError multipleElseError("Multiple 'else/otherwise' blocks are not allowed", tokens[currentTokenIndex].line, tokens[currentTokenIndex].column);
    SyntaxError multipleFinallyError("Multiple 'finally/anyway' blocks are not allowed", tokens[currentTokenIndex].line, tokens[currentTokenIndex].column);

    consume(TokenType::OPEN_BRACE, "", "Expected '{' after 'try/attempt'");
    std::unique_ptr<ASTNode> body = parseBlock();
    consume(TokenType::KEYWORD, "catch", "Expected 'catch/except' after 'try/attempt' block");
    bool openParen = match(TokenType::OPEN_PAREN);
    consume(TokenType::IDENTIFIER, "", "Expected identifier after 'catch/except'");
    std::string_view catchIdentifier = tokens[currentTokenIndex - 1].value;
    if (openParen)
        consume(TokenType::CLOSE_PAREN, "", "Expected ')' after 'catch/except' identifier");
    consume(TokenType::OPEN_BRACE, "", "Expected '{' after 'catch/except' identifier");
    std::unique_ptr<ASTNode> catchBody = parseBlock();

    // Parse optional 'finally' and 'else' blocks, order is not important
    std::unique_ptr<ASTNode> finallyBlock = nullptr;
    std::unique_ptr<ASTNode> elseBlock = nullptr;
    while (match(TokenType::KEYWORD, "finally", false) || match(TokenType::KEYWORD, "otherwise", false))
    {
        if (match(TokenType::KEYWORD, "finally"))
        {
            if (finallyBlock)
            {
                TraceGuard guard(multipleFinallyError, filename, "Parser::parseTryCatchStatement", tokens[currentTokenIndex].line);
                throw multipleFinallyError;
            }
            consume(TokenType::OPEN_BRACE, "", "Expected '{' after 'finally/anyway'");
            finallyBlock = parseBlock();
        }
        else
        {
            consume(TokenType::KEYWORD, "otherwise");
            if (elseBlock)
            {
                TraceGuard guard(multipleElseError, filename, "Parser::parseTryCatchStatement", tokens[currentTokenIndex].line);
                throw multipleElseError;
            }
            consume(TokenType::OPEN_BRACE, "", "Expected '{' after 'else/otherwise'");
            elseBlock = parseBlock();
        }
    }

    return std::make_unique<TryCatchStatement>(std::move(body), catchIdentifier, std::move(catchBody), std::move(finallyBlock), std::move(elseBlock), scopeManager.currentScope());
}

std::unique_ptr<ASTNode> Parser::parseThrowStatement()
{
    bool openParen = match(TokenType::OPEN_PAREN);
    std::unique_ptr<ASTNode> value = parseExpression();
    if (openParen)
        consume(TokenType::CLOSE_PAREN, "", "Expected ')' after 'throw' value");
    return std::make_unique<ThrowStatement>(std::move(value), scopeManager.currentScope());
}

std::unique_ptr<ASTNode> Parser::parseReturnStatement()
{
    std::unique_ptr<ASTNode> value = parseExpression();
    return std::make_unique<ReturnStatement>(std::move(value), scopeManager.currentScope());
}

std::unique_ptr<ASTNode> Parser::parseForLoop()
{
    std::vector<Parameter> parameters;  // Vector to store the parameters
    parseParameters(parameters, false); // Parse the parameters
    consume(TokenType::KEYWORD, "in", "Expected 'in' after 'for' parameters");
    std::unique_ptr<ASTNode> iterable = parseExpression(); // Parse the iterable
    consume(TokenType::OPEN_BRACE, "", "Expected '{' after 'for/foreach' iterable");
    std::unique_ptr<ASTNode> body = parseBlock(); // Parse the body
    return std::make_unique<ForLoop>(std::move(parameters), std::move(iterable), std::move(body));
}

std::unique_ptr<ASTNode> Parser::parseWhileLoop()
{
    bool openParen = match(TokenType::OPEN_PAREN);
    std::unique_ptr<ASTNode> condition = parseExpression(); // Parse the condition
    if (openParen)
        consume(TokenType::CLOSE_PAREN, "", "Expected ')' after 'while/aslongas' condition");
    consume(TokenType::OPEN_BRACE, "", "Expected '{' after 'while/aslongas' condition");
    std::unique_ptr<ASTNode> body = parseBlock(); // Parse the body
    return std::make_unique<WhileLoop>(std::move(condition), std::move(body));
}

std::unique_ptr<ASTNode> Parser::parseAssignmentOrCall()
{
    SyntaxError unexpectedTokenError("Unexpected token '" + lower(tokens[currentTokenIndex].value) + "'", tokens[currentTokenIndex].line, tokens[currentTokenIndex].column);
    TraceGuard guard(unexpectedTokenError, filename, "Parser::parseAssignmentOrCall", tokens[currentTokenIndex].line);

    std::vector<std::string_view> identifierParts = parseFullIdentifier();

    if (match(TokenType::KEYWORD, "is"))
    {
        std::unique_ptr<ASTNode> value = parseExpression();
        return std::make_unique<VariableAssignment>(identifierParts, std::move(value), scopeManager.currentScope());
    }
    else if (match(TokenType::OPEN_PAREN))
    {
        return parseFunctionCall(identifierParts);
    }
    else
    {
        throw unexpectedTokenError;
    }
}

std::unique_ptr<ASTNode> Parser::parseFunctionCall(const std::vector<std::string_view> &identifierParts)
{
    std::vector<std::unique_ptr<ASTNode>> args;
    std::unordered_map<std::string_view, std::unique_ptr<ASTNode>> kwargs;

    if (!match(TokenType::CLOSE_PAREN)) // If not empty argument list
    {
        bool seenKwarg = false;
        do
        {
            if (peekMatch(TokenType::IDENTIFIER) && peekMatch(TokenType::KEYWORD, 1, "is")) //
            {
                std::string_view key = tokens[currentTokenIndex].value; // Get the identifier
                advance();                                              // Skip the identifier
                advance();                                              // Skip the 'is' keyword
                std::unique_ptr<ASTNode> value = parseExpression();     // Parse value
                kwargs[key] = std::move(value);
                seenKwarg = true; // We've now seen a kwarg
            }
            else
            {
                if (seenKwarg)
                {
                    SyntaxError argumentAfterKwargError("Positional arguments cannot follow keyword arguments", tokens[currentTokenIndex].line, tokens[currentTokenIndex].column);
                    TraceGuard guard(argumentAfterKwargError, filename, "Parser::parseFunctionCall", tokens[currentTokenIndex].line);
                    throw argumentAfterKwargError;
                }
                // Handle positional argument without identifier
                args.push_back(parseExpression());
            }
        } while (match(TokenType::COMMA) || match(TokenType::KEYWORD, "also"));

        // Ensure the closing parenthesis is consumed
        consume(TokenType::CLOSE_PAREN, "", "Expected ')' after function arguments");
    }

    // Return the FunctionCall node, combining positional and keyword arguments
    return std::make_unique<FunctionCall>(identifierParts, std::move(args), std::move(kwargs), scopeManager.currentScope());
}

std::vector<std::string_view> Parser::parseFullIdentifier()
{
    SyntaxError missingIdentifierError("Expected identifier after '.'", tokens[currentTokenIndex].line, tokens[currentTokenIndex].column);
    TraceGuard guard(missingIdentifierError, filename, "Parser::parseFullIdentifier", tokens[currentTokenIndex].line);

    std::vector<std::string_view> parts;
    if (match(TokenType::IDENTIFIER) ||
        match(TokenType::KEYWORD, "outer") ||
        match(TokenType::KEYWORD, "global") ||
        match(TokenType::KEYWORD, "this") ||
        match(TokenType::KEYWORD, "parent") ||
        match(TokenType::KEYWORD, "self") ||
        match(TokenType::KEYWORD, "super"))
    {
        parts.push_back(tokens[currentTokenIndex - 1].value);

        while (match(TokenType::DOT))
        {
            if (!match(TokenType::IDENTIFIER) &&
                !match(TokenType::KEYWORD, "outer") &&
                !match(TokenType::KEYWORD, "global") &&
                !match(TokenType::KEYWORD, "this") &&
                !match(TokenType::KEYWORD, "parent") &&
                !match(TokenType::KEYWORD, "self") &&
                !match(TokenType::KEYWORD, "super"))
            {
                throw missingIdentifierError;
            }
            parts.push_back(tokens[currentTokenIndex - 1].value);
        }
    }
    return parts;
}

std::unique_ptr<ASTNode> Parser::parseVariableDeclaration()
{
    SyntaxError missingVariableNameError("Expected variable name", tokens[currentTokenIndex].line, tokens[currentTokenIndex].column);
    SyntaxError expectedTypeError("Expected type after 'oftype'", tokens[currentTokenIndex].line, tokens[currentTokenIndex].column);

    // Parse the full identifier instead of just a single identifier
    std::vector<std::string_view> identifierParts = parseFullIdentifier();

    // Check if we have at least one part in the identifier
    if (identifierParts.empty())
    {
        TraceGuard guard(missingVariableNameError, filename, "Parser::parseVariableDeclaration", tokens[currentTokenIndex].line);
        throw missingVariableNameError;
    }

    std::string_view type = "any"; // Default type is 'any'
    if (match(TokenType::KEYWORD, "oftype"))
    {
        if (match(TokenType::TYPE, "") || match(TokenType::IDENTIFIER, ""))
        {
            type = tokens[currentTokenIndex - 1].value;
        }
        else
        {
            TraceGuard guard(expectedTypeError, filename, "Parser::parseVariableDeclaration", tokens[currentTokenIndex].line);
            throw expectedTypeError;
        }
    }

    std::shared_ptr<Scope> scope = scopeManager.currentScope();

    if (scope->type == ScopeType::DecoratorPaired && scope->name == "declareonly")
    {
        return std::make_unique<VariableDeclaration>(identifierParts, type, nullptr, scope);
    }

    consume(TokenType::KEYWORD, "IS", "Expected '=', 'is' or 'set' after variable declaration");
    std::unique_ptr<ASTNode> initializer = parseExpression(); // Parse the initializer expression

    return std::make_unique<VariableDeclaration>(identifierParts, type, std::move(initializer), scope);
}

void Parser::parseParameters(std::vector<Parameter> &parameters, bool canBeOptional)
{
    SyntaxError expectedTypeError("Expected type after 'oftype'", tokens[currentTokenIndex].line, tokens[currentTokenIndex].column);

    bool openedParen = match(TokenType::OPEN_PAREN, "");
    bool isOptional = canBeOptional && match(TokenType::KEYWORD, "optional");
    // Parse parameter (name, type, optional default value)
    if (match(TokenType::KEYWORD, "var", false))
    {
        consume(TokenType::KEYWORD, "var", "Expected 'var' before parameter name");
    }
    consume(TokenType::IDENTIFIER, "", "Expected parameter name");
    std::string_view paramName = tokens[currentTokenIndex - 1].value;

    std::string_view paramType = "any"; // Default type is 'any'
    if (match(TokenType::KEYWORD, "oftype"))
    {
        if (match(TokenType::TYPE, "") || match(TokenType::IDENTIFIER, ""))
        {
            paramType = tokens[currentTokenIndex - 1].value;
        }
        else
        {
            TraceGuard guard(expectedTypeError, filename, "Parser::parseParameters", tokens[currentTokenIndex].line);
            throw expectedTypeError;
        }
    }

    std::unique_ptr<ASTNode> defaultValue = nullptr;
    if (canBeOptional)
    {
        if (match(TokenType::KEYWORD, "default") || match(TokenType::KEYWORD, "is"))
        {
            isOptional = true;
            defaultValue = parseExpression();
        }
    }

    // Add the parameter as a tuple of name, isOptional, type and default value
    parameters.push_back({paramName, isOptional, paramType, std::move(defaultValue)});

    if (openedParen)
    {
        consume(TokenType::CLOSE_PAREN, "", "Expected ')' after parameter declaration");
    }

    if (match(TokenType::KEYWORD, "also") || match(TokenType::COMMA))
    {
        return parseParameters(parameters);
    }
}

std::unique_ptr<ASTNode> Parser::parseActionDeclaration()
{
    SyntaxError expectedTypeError("Expected return type after 'returns'", tokens[currentTokenIndex].line, tokens[currentTokenIndex].column);
    TraceGuard guard(expectedTypeError, filename, "Parser::parseActionDeclaration", tokens[currentTokenIndex].line);

    consume(TokenType::IDENTIFIER, "", "Expected action name");
    std::string_view name = tokens[currentTokenIndex - 1].value;

    // Check if function extends a parent object
    std::string_view parentName = ""; // Default parent object is empty
    if (match(TokenType::KEYWORD, "extends"))
    {
        consume(TokenType::IDENTIFIER, "", "Expected parent object name");
        parentName = tokens[currentTokenIndex - 1].value;
    }

    std::vector<Parameter> parameters; // Vector to store the parameters
    if (match(TokenType::KEYWORD, "with"))
    {
        parseParameters(parameters); // Parse the parameters
    }

    std::string_view returnType = "any"; // Default return type is 'any'
    if (match(TokenType::KEYWORD, "returns"))
    {
        if (match(TokenType::TYPE, "") || match(TokenType::IDENTIFIER, ""))
        {
            returnType = tokens[currentTokenIndex - 1].value;
        }
        else
        {
            throw expectedTypeError;
        }
    }

    consume(TokenType::OPEN_BRACE, "", "Expected '{' to start action body");

    scopeManager.enterScope(name, ScopeType::Local); // Enter a new scope for the action
    std::unique_ptr<ASTNode> body = parseBlock();    // Parse the action body
    scopeManager.exitScope();                        // Exit the action scope

    return std::make_unique<ActionDeclaration>(name, parentName, std::move(parameters), returnType, std::move(body), scopeManager.currentScope());
}

std::unique_ptr<ASTNode> Parser::parseObjectDeclaration()
{
    SyntaxError nestedObjectsError("Object declarations are not allowed inside other objects", tokens[currentTokenIndex].line, tokens[currentTokenIndex].column);
    TraceGuard guard(nestedObjectsError, filename, "Parser::parseObjectDeclaration", tokens[currentTokenIndex].line);

    if (scopeManager.currentScope() && scopeManager.currentScope()->type != ScopeType::Global)
    {
        TraceGuard guard(nestedObjectsError, filename, "Parser::parseStatement", tokens[currentTokenIndex].line);
        throw nestedObjectsError;
    }

    consume(TokenType::IDENTIFIER, "", "Expected object name");
    std::string_view name = tokens[currentTokenIndex - 1].value;

    std::string_view parentName = ""; // Default parent object is empty
    if (match(TokenType::KEYWORD, "extends"))
    {
        consume(TokenType::IDENTIFIER, "", "Expected parent object name");
        parentName = tokens[currentTokenIndex - 1].value;
    }

    auto object = std::make_unique<ObjectDeclaration>(name, parentName, scopeManager.currentScope());

    scopeManager.enterScope(name, ScopeType::ObjectPaired); // Enter a new scope for the object
    consume(TokenType::OPEN_BRACE, "", "Expected '{' to start object body");
    while (!match(TokenType::CLOSE_BRACE))
    {
        object->statements.push_back(parseStatement());
    }
    scopeManager.exitScope(); // Exit the object scope

    return object;
}

std::unique_ptr<ASTNode> Parser::parseBlock()
{
    auto block = std::make_unique<Block>(scopeManager.currentScope());
    while (!match(TokenType::CLOSE_BRACE))
    {
        block->statements.push_back(parseStatement());
    }

    return block;
}

std::unique_ptr<ASTNode> Parser::parseExpression(int precedence)
{
    // Start by parsing the primary expression (literals, variables, parenthesized expressions, etc.)
    std::unique_ptr<ASTNode> left = parsePrimary();

    while (true)
    {
        // If we are at the end of the tokens, break the loop
        if (currentTokenIndex >= tokens.size())
            break;

        // Look at the current token to see if it is a binary operator
        Token current = tokens[currentTokenIndex];

        std::unordered_set<TokenType> stopTokens = {
            TokenType::CLOSE_PAREN, TokenType::CLOSE_BRACE, TokenType::COMMA,
            TokenType::CLOSE_BRACKET, TokenType::EOF_, TokenType::IDENTIFIER,
            TokenType::OPEN_PAREN, TokenType::OPEN_BRACE, TokenType::OPEN_BRACKET};

        // Check if we are still inside the current expression (stop parsing if something new starts)
        if (stopTokens.find(current.type) != stopTokens.end() ||
            (current.type == TokenType::KEYWORD && current.value != "also"))
        {
            break;
        }

        // Get the precedence of the current token
        int tokenPrecedence = getPrecedence(current);

        // If the current token has a lower precedence than the current expression, break out
        if (tokenPrecedence < precedence)
            break;

        // Check if it's the `nonnull` operator
        if (current.type == TokenType::OPERATOR && current.value == "nonnull")
        {
            // Consume the 'nonnull' token
            advance();

            // Parse the right-hand side, which is the fallback value
            std::unique_ptr<ASTNode> right = parseExpression(tokenPrecedence + 1);

            // Combine the left and right into a `NonNullExpression`
            left = std::make_unique<NonNullExpression>(std::move(left), std::move(right), scopeManager.currentScope());

            // Continue parsing the rest of the expression
            continue;
        }

        // For all other operators
        advance();
        std::unique_ptr<ASTNode> right = parseExpression(tokenPrecedence + 1);

        // Combine the left and right into a binary expression node
        left = std::make_unique<BinaryExpression>(std::move(left), current, std::move(right), scopeManager.currentScope());
    }

    return left; // Return the resulting expression
}

std::unique_ptr<ASTNode> Parser::parsePrimary()
{
    Token current = tokens[currentTokenIndex];
    SyntaxError unexpectedTokenError("Unexpected token '" + lower(current.value) + "' in expression", current.line, current.column);
    TraceGuard guard(unexpectedTokenError, filename, "Parser::parsePrimary", tokens[currentTokenIndex].line);

    // Check if it's a literal (e.g., numbers, strings, booleans)
    if (match(TokenType::INTEGER) || match(TokenType::FLOAT) || match(TokenType::STRING) || match(TokenType::BOOLEAN) || match(TokenType::NULL_))
    {
        return std::make_unique<Literal>(current, scopeManager.currentScope());
    }
    else if (match(TokenType::OPEN_PAREN))
    {
        // Parenthesized expression
        std::unique_ptr<ASTNode> expression = parseExpression();
        consume(TokenType::CLOSE_PAREN, "", "Expected ')' after expression");
        return expression;
    }
    else if (match(TokenType::OPEN_BRACE))
    {
        // Block expression
        return parseBlock();
    }
    else if (match(TokenType::OPEN_BRACKET))
    {
        // List literal
        std::vector<std::unique_ptr<ASTNode>> elements;
        if (!match(TokenType::CLOSE_BRACKET)) // If it's not an empty list
        {
            do
            {
                elements.push_back(parseExpression());
            } while (match(TokenType::COMMA));
            consume(TokenType::CLOSE_BRACKET, "", "Expected ']' after list elements");
        }
        return std::make_unique<ListLiteral>(std::move(elements), scopeManager.currentScope());
    }

    // Otherwise, handle identifiers (variables or function calls)
    if (match(TokenType::KEYWORD, "outer", false) ||
        match(TokenType::KEYWORD, "global", false) ||
        match(TokenType::KEYWORD, "this", false) ||
        match(TokenType::KEYWORD, "parent", false) ||
        match(TokenType::KEYWORD, "self", false) ||
        match(TokenType::KEYWORD, "super", false) ||
        match(TokenType::IDENTIFIER, "", false))
    {
        std::vector<std::string_view> identifierParts = parseFullIdentifier();
        if (match(TokenType::OPEN_PAREN))
        {
            // Function call
            return parseFunctionCall(identifierParts);
        }
        else
        {
            // Variable reference
            return std::make_unique<VariableReference>(identifierParts, scopeManager.currentScope());
        }
    }

    throw unexpectedTokenError;
}

int Parser::getPrecedence(const Token &token)
{
    if (token.type == TokenType::OPERATOR)
    {
        if (token.value == "plus" || token.value == "minus")
            return 1;
        if (token.value == "multiply" || token.value == "divide" || token.value == "modulo")
            return 2;
        if (token.value == "equals" || token.value == "notequals")
            return 3;
        if (token.value == "nonnull")
            return 4; // Set precedence for `nonnull` operator
    }
    return 0;
}
