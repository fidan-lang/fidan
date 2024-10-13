// Copyright (c) AppSolves (Kaan Gönüldinc). All rights reserved.
// See LICENSE file in the project root for full license information.

#include "headers/tokenizer.h"
#include "headers/errors.h"

Tokenizer::Tokenizer(const std::string &source, const std::string &filename)
    : source(source), filename(filename), position(0), line(1), column(1) {}

std::vector<Token> Tokenizer::tokenize()
{
    std::vector<Token> tokens;
    const size_t estimatedTokenCount = source.size() / 7; // Assuming average token length is 7 characters
    tokens.reserve(estimatedTokenCount);
    while (true)
    {
        Token token = nextToken();
        if (token.type == TokenType::EOF_)
        {
            break;
        }
        tokens.push_back(std::move(token));
    }
    return tokens;
}

inline char Tokenizer::currentChar() const
{
    return (position >= source.size()) ? '\0' : source[position];
}

inline char Tokenizer::peekChar() const
{
    return (position + 1 >= source.size()) ? '\0' : source[position + 1];
}

inline void Tokenizer::advance()
{
    if (position >= source.size())
    {
        return;
    }

    char current = currentChar();
    if (current == '\n' || current == '\0')
    {
        line++;
        column = 1;
    }
    else
    {
        column++;
    }
    position++;
}

inline void Tokenizer::skipWhitespace()
{
    while (std::isspace(static_cast<unsigned char>(currentChar())))
    {
        advance();
    }
}

Token Tokenizer::processComment()
{
    SyntaxError unterminatedBlockComment("Unterminated block comment", line, column);
    SyntaxError unexpectedCharacter("Unexpected character", line, column);

    char current = currentChar();
    if (current == '#')
    {
        while (current != '\n' && current != '\0')
        {
            advance();
            current = currentChar();
        }
        return nextToken();
    }
    else if (current == '/')
    {
        advance();
        while (true)
        {
            current = currentChar();
            if (current == '\0')
            {
                TraceGuard guard(unterminatedBlockComment, filename, "Tokenizer::processComment", line);
                throw unterminatedBlockComment;
            }
            if (current == '/' && peekChar() == '#')
            {
                advance();
                advance();
                return nextToken();
            }
            advance();
        }
    }
    else
    {
        TraceGuard guard(unexpectedCharacter, filename, "Tokenizer::processComment", line);
        throw unexpectedCharacter;
    }
}

Token Tokenizer::nextToken()
{
    SyntaxError unexpectedCharacter("Unexpected character", line, column);
    TraceGuard guard(unexpectedCharacter, filename, "Tokenizer::nextToken", line);

    skipWhitespace();
    if (position >= source.size())
    {
        return {TokenType::EOF_, "", line, column};
    }

    char current = currentChar();
    if (std::isalpha(static_cast<unsigned char>(current)))
    {
        return identifier();
    }
    else if (std::isdigit(static_cast<unsigned char>(current)))
    {
        return number();
    }
    else if (current == '"')
    {
        return string();
    }
    else
    {
        advance();
        char actualCurrent = currentChar();
        switch (current)
        {
        case '=':
            if (actualCurrent == '=')
            {
                advance();
                return {TokenType::OPERATOR, "EQUALS", line, column};
            }
            return {TokenType::KEYWORD, "IS", line, column};
        case '!':
            if (actualCurrent == '=')
            {
                advance();
                return {TokenType::OPERATOR, "NOTEQUALS", line, column};
            }
            return {TokenType::OPERATOR, "NOT", line, column};
        case '>':
            if (actualCurrent == '=')
            {
                advance();
                return {TokenType::OPERATOR, "GREATERTHANOREQUALS", line, column};
            }
            return {TokenType::OPERATOR, "GREATERTHAN", line, column};
        case '<':
            if (actualCurrent == '=')
            {
                advance();
                return {TokenType::OPERATOR, "LESSTHANOREQUALS", line, column};
            }
            return {TokenType::OPERATOR, "LESSTHAN", line, column};
        case '(':
            return {TokenType::OPEN_PAREN, "(", line, column};
        case ')':
            return {TokenType::CLOSE_PAREN, ")", line, column};
        case '{':
            return {TokenType::OPEN_BRACE, "{", line, column};
        case '}':
            return {TokenType::CLOSE_BRACE, "}", line, column};
        case '[':
            return {TokenType::OPEN_BRACKET, "[", line, column};
        case ']':
            return {TokenType::CLOSE_BRACKET, "]", line, column};
        case ',':
            return {TokenType::COMMA, ",", line, column};
        case '+':
            if (actualCurrent == '=')
            {
                advance();
                return {TokenType::OPERATOR, "PLUSEQUALS", line, column};
            }
            else if (actualCurrent == '+')
            {
                advance();
                return {TokenType::OPERATOR, "INCREMENT", line, column};
            }
            return {TokenType::OPERATOR, "PLUS", line, column};
        case '-':
            if (actualCurrent == '=')
            {
                advance();
                return {TokenType::OPERATOR, "MINUSEQUALS", line, column};
            }
            else if (actualCurrent == '-')
            {
                advance();
                return {TokenType::OPERATOR, "DECREMENT", line, column};
            }
            return {TokenType::OPERATOR, "MINUS", line, column};
        case '*':
            if (actualCurrent == '=')
            {
                advance();
                return {TokenType::OPERATOR, "MULTIPLYEQUALS", line, column};
            }
            return {TokenType::OPERATOR, "MULTIPLY", line, column};
        case '/':
            if (actualCurrent == '=')
            {
                advance();
                return {TokenType::OPERATOR, "DIVIDEEQUALS", line, column};
            }
            return {TokenType::OPERATOR, "DIVIDE", line, column};
        case '%':
            if (actualCurrent == '=')
            {
                advance();
                return {TokenType::OPERATOR, "MODULOEQUALS", line, column};
            }
            return {TokenType::OPERATOR, "MODULO", line, column};
        case '&':
            if (actualCurrent == '&')
            {
                advance();
                return {TokenType::OPERATOR, "AND", line, column};
            }
            return {TokenType::OPERATOR, "BITWISEAND", line, column};
        case '|':
            if (actualCurrent == '|')
            {
                advance();
                return {TokenType::OPERATOR, "OR", line, column};
            }
            return {TokenType::OPERATOR, "BITWISEOR", line, column};
        case '^':
            return {TokenType::OPERATOR, "BITWISEXOR", line, column};
        case '~':
            return {TokenType::OPERATOR, "BITWISENOT", line, column};
        case '?':
            return {TokenType::OPERATOR, "TERNARY", line, column};
        case ':':
            return {TokenType::COLON, "COLON", line, column};
        case '.':
            return {TokenType::DOT, "DOT", line, column};
        case '@':
        {
            size_t start = position;
            while (std::isalnum(static_cast<unsigned char>(currentChar())))
            {
                advance();
            }
            std::string_view value(&source[start], position - start);
            return {TokenType::DECORATOR, value, line, column};
        }
        case '#':
            return processComment();
        default:
            throw unexpectedCharacter;
        }
    }
}

Token Tokenizer::identifier()
{
    size_t start = position;
    char current = currentChar();
    while (std::isalnum(static_cast<unsigned char>(current)) || current == '_')
    {
        advance();
        current = currentChar();
    }
    std::string_view value(&source[start], position - start);

    static const std::unordered_map<std::string_view, std::vector<std::string_view>> keywords = {
        {"OBJECT", {"object", "class"}}, {"PARENT", {"parent", "super"}}, {"THIS", {"this", "self"}}, {"OUTER", {"outer", "nonlocal"}}, {"GLOBAL", {"global"}}, {"OFTYPE", {"oftype"}}, {"WHEN", {"when", "if"}}, {"OTHERWISE", {"otherwise", "else"}}, {"ACTION", {"action", "function"}}, {"ATTEMPT", {"attempt", "try"}}, {"CATCH", {"catch", "except"}}, {"FINALLY", {"finally", "anyway"}}, {"WHILE", {"while", "aslongas"}}, {"FOR", {"for", "foreach"}}, {"IN", {"in"}}, {"IS", {"is", "set"}}, {"VAR", {"var", "variable"}}, {"TYPE", {"string", "integer", "float", "boolean", "dynamic", "list", "dictionary", "null"}}, {"IMPORT", {"import"}}, {"CONST", {"const"}}, {"TRUE", {"true"}}, {"FALSE", {"false"}}, {"BREAK", {"break", "stop"}}, {"CONTINUE", {"continue", "next"}}, {"WITH", {"with"}}, {"OPTIONAL", {"optional"}}, {"DEFAULT", {"default"}}, {"EXTENDS", {"extends", "inherits", "includes"}}, {"THROW", {"throw", "raise"}}, {"ALSO", {"also"}}, {"RETURNS", {"returns"}}, {"RETURN", {"return"}}};

    static const std::unordered_map<std::string_view, std::vector<std::string_view>> operator_aliases = {
        {"PLUS", {"add", "plus"}}, {"MINUS", {"subtract", "minus"}}, {"MULTIPLY", {"multiplyby", "times"}}, {"DIVIDE", {"divideby", "over"}}, {"MODULO", {"modulo"}}, {"AND", {"and"}}, {"OR", {"or"}}, {"NOT", {"not"}}, {"NONNULL", {"nonnull"}}, {"EQUALS", {"equals"}}, {"NOTEQUALS", {"notequals"}}, {"GREATERTHAN", {"greaterthan"}}, {"LESSTHAN", {"lessthan"}}, {"GREATERTHANOREQUALS", {"greaterthanorequals"}}, {"LESSTHANOREQUALS", {"lessthanorequals"}}, {"PLUSEQUALS", {"plusequals"}}, {"MINUSEQUALS", {"minusequals"}}, {"MULTIPLYEQUALS", {"multiplyequals"}}, {"DIVIDEEQUALS", {"divideequals"}}, {"MODULOEQUALS", {"moduloequals"}}, {"TERNARY", {"ternary"}}, {"BITWISEAND", {"bitwiseand"}}, {"BITWISEOR", {"bitwiseor"}}, {"BITWISEXOR", {"bitwisexor"}}, {"BITWISENOT", {"bitwisenot"}}, {"INCREMENT", {"increment"}}, {"DECREMENT", {"decrement"}}};

    for (const auto &keyword : keywords)
    {
        for (const std::string_view &word : keyword.second)
        {
            if (value == word)
            {
                if (keyword.first == "TYPE")
                {
                    return {TokenType::TYPE, value, line, column};
                }
                return {TokenType::KEYWORD, keyword.first, line, column};
            }
        }
    }

    for (const auto &alias : operator_aliases)
    {
        for (const std::string_view &word : alias.second)
        {
            if (value == word)
            {
                return {TokenType::OPERATOR, alias.first, line, column};
            }
        }
    }

    return {TokenType::IDENTIFIER, value, line, column};
}

Token Tokenizer::number()
{
    SyntaxError invalidNumberFormat("Invalid number format: multiple decimal points", line, column);
    TraceGuard guard(invalidNumberFormat, filename, "Tokenizer::number", line);

    size_t start = position;
    bool decimalPointEncountered = false;
    char current = currentChar();

    while (std::isdigit(static_cast<unsigned char>(current)))
    {
        advance();
        current = currentChar();
    }

    if (current == '.')
    {
        decimalPointEncountered = true;
        advance();
        while (std::isdigit(static_cast<unsigned char>(current)))
        {
            advance();
            current = currentChar();
        }
    }

    if (current == '.')
    {
        throw invalidNumberFormat;
    }

    std::string_view value(&source[start], position - start);

    if (decimalPointEncountered)
    {
        return {TokenType::FLOAT, value, line, column};
    }

    return {TokenType::INTEGER, value, line, column};
}

Token Tokenizer::string()
{
    SyntaxError unterminatedString("Unterminated string literal", line, column);
    TraceGuard guard(unterminatedString, filename, "Tokenizer::string", line);

    advance();
    size_t start = position;
    char current = currentChar();

    while (current != '"' && current != '\0' && current != '\n')
    {
        if (current == '\\')
        {
            advance();
            current = currentChar();
            if (current == '"' || current == '\\')
            {
                advance();
            }
        }
        else
        {
            advance();
        }
        current = currentChar();
    }

    current = currentChar();
    if (current == '\0' || current == '\n')
    {
        throw unterminatedString;
    }

    advance();
    std::string_view value(&source[start], position - start);
    return {TokenType::STRING, value, line, column};
}