// Copyright (c) Kaan Gönüldinc (AppSolves). All rights reserved.
// See LICENSE file in the project root for full license information.

#include "headers/helpers.h"
#include <iostream>

// Convert string to uppercase
std::string upper(std::string_view input)
{
    std::string upperStr(input);
    std::for_each(upperStr.begin(), upperStr.end(), [](char &c)
                  { c = std::toupper(c); });
    return upperStr;
}

// Convert string to lowercase
std::string lower(std::string_view input)
{
    std::string lowerStr(input);
    std::for_each(lowerStr.begin(), lowerStr.end(), [](char &c)
                  { c = std::tolower(c); });
    return lowerStr;
}

// Join a vector of strings with a delimiter
std::string join(const std::vector<std::string_view> &parts, const std::string_view delimiter)
{
    std::string result;
    for (size_t i = 0; i < parts.size(); i++)
    {
        result += parts[i];
        if (i < parts.size() - 1)
        {
            result += delimiter;
        }
    }
    return result;
}

// Print a string
const void print(const std::string &text, int numNewLines)
{
    std::cout << text;
    for (int i = 0; i < numNewLines; i++)
    {
        std::cout << std::endl;
    }
}

// Check if brackets are balanced
std::optional<BracketError> checkBalancedBrackets(const std::vector<Token> &tokens)
{
    std::stack<Token> bracketStack;
    static const std::unordered_map<char, char> matchingBracket = {
        {')', '('},
        {'}', '{'},
        {']', '['}};

    for (const Token &token : tokens)
    {
        char ch = token.value[0];
        if (matchingBracket.count(ch))
        {
            // If it's a closing bracket
            if (bracketStack.empty() || bracketStack.top().value[0] != matchingBracket.at(ch))
            {
                return BracketError{token, false}; // Unmatched closing bracket
            }
            bracketStack.pop(); // Pop the matching opening bracket
        }
        else if (ch == '(' || ch == '{' || ch == '[')
        {
            // If it's an opening bracket
            bracketStack.push(token);
        }
    }

    if (!bracketStack.empty())
    {
        Token unmatchedToken = bracketStack.top();
        return BracketError{unmatchedToken, true}; // Unmatched opening bracket
    }

    return std::nullopt; // All brackets matched
}