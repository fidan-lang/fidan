// Copyright (c) Kaan Gönüldinc (AppSolves). All rights reserved.
// See LICENSE file in the project root for full license information.

#ifndef FIDAN_TOKENIZER_H
#define FIDAN_TOKENIZER_H

#include <string>
#include <string_view>
#include <vector>
#include <unordered_map>
#include "errors.h"

// Enum class representing different types of tokens
enum class TokenType
{
    TYPE,
    INTEGER,
    FLOAT,
    STRING,
    BOOLEAN,
    LIST,
    DICTIONARY,
    NULL_,
    IDENTIFIER,
    KEYWORD,
    DECORATOR,
    OPERATOR,
    OPEN_PAREN,
    CLOSE_PAREN,
    OPEN_BRACE,
    CLOSE_BRACE,
    OPEN_BRACKET,
    CLOSE_BRACKET,
    COMMA,
    DOT,
    COLON,
    EOF_
};

// Struct representing a token with its type, value, line, and column information
struct Token
{
    TokenType type;         // Type of the token
    std::string_view value; // Value of the token
    int line;               // Line number where the token is found
    int column;             // Column number where the token is found

    // Constructor to initialize a token
    Token(TokenType t, std::string_view v, int l, int c)
        : type(t), value(v), line(l), column(c)
    {
        // Ensure line and column numbers are positive
        if (l < 1 || c < 1)
        {
            RuntimeError error("Line and column must be positive", l, c);
            TraceGuard guard(error, "<internal>", "Tokenizer::Token", l);
        }
    }
};

// Class responsible for tokenizing a source string
class Tokenizer
{
public:
    // Constructor to initialize the tokenizer with source code and filename
    explicit Tokenizer(const std::string &source, const std::string &filename);

    // Function to tokenize the source code and return a vector of tokens
    std::vector<Token> tokenize();

private:
    const std::string &source;   // Source code to tokenize
    const std::string &filename; // Filename of the source code
    size_t position;             // Current position in the source code
    int line;                    // Current line number
    int column;                  // Current column number

    // Function to get the current character in the source code
    inline char currentChar() const;

    // Function to peek at the next character in the source code
    inline char peekChar() const;

    // Function to advance the current position in the source code
    inline void advance();

    // Function to skip whitespace characters in the source code
    inline void skipWhitespace();

    // Function to process comments in the source code
    Token processComment();

    // Function to get the next token from the source code
    Token nextToken();

    // Function to process identifiers in the source code
    Token identifier();

    // Function to process numbers in the source code
    Token number();

    // Function to process strings in the source code
    Token string();
};

#endif // FIDAN_TOKENIZER_H