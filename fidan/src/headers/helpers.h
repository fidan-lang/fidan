// Copyright (c) Kaan Gönüldinc (AppSolves). All rights reserved.
// See LICENSE file in the project root for full license information.

#ifndef FIDAN_HELPERS_H
#define FIDAN_HELPERS_H

// Include necessary headers
#include "parser.h"
#include <string>
#include <algorithm>
#include <variant>
#include <unordered_map>
#include <functional>
#include <vector>
#include <stdexcept>
#include <memory>
#include <cmath>
#include <stack>
#include <optional>

// Define DEBUG_MODE
#ifdef DEBUG_MODE
const bool isDebugMode = true;
#else
const bool isDebugMode = false;
#endif

// Functions to convert a string to upper or lower case
std::string upper(std::string_view input);
std::string lower(std::string_view input);
// Function to join a vector of string views with a delimiter
std::string join(const std::vector<std::string_view> &parts, const std::string_view delimiter);
// Function to print a string
const void print(const std::string &text, int numNewLines = 1);

// Class to represent a `BracketError`, which is thrown when there is an unmatched bracket in the code
struct BracketError
{
    Token token;    // The unmatched token
    bool isOpening; // True if the token is an opening bracket, false if it's a closing bracket
};

// Function to check if the brackets in the code are balanced
std::optional<BracketError> checkBalancedBrackets(const std::vector<Token> &tokens);

#endif // FIDAN_HELPERS_H