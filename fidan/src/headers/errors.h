// Copyright (c) AppSolves (Kaan Gönüldinc). All rights reserved.
// See LICENSE file in the project root for full license information.

#ifndef FIDAN_ERRORS_H
#define FIDAN_ERRORS_H

#include <exception>
#include <string>
#include <vector>
#include <sstream>
#include <unordered_map>

// Preprocess the source code into a map for O(1) line retrieval
std::unordered_map<int, std::string> preprocessSource(const std::string_view source) noexcept;

// This function retrieves the source line from the preprocessed map
std::string getSourceLine(int lineNumber, const std::unordered_map<int, std::string> &sourceMap) noexcept;

enum class FidanErrorType
{
    SyntaxError,
    RuntimeError,
    ValueError,
    TypeError,
    DeclarationError
};

// Base class for exceptions in Fidan language
class FidanException : public std::exception
{
protected:
    std::string message;
    int line, column;
    std::vector<std::string> traceback;

public:
    FidanException(const std::string &msg, int line = -1, int column = -1)
        : message(msg), line(line), column(column) {}

    const char *what() const noexcept override
    {
        return message.c_str();
    }

    int getLine() const { return line; }
    int getColumn() const { return column; }
    // Method to get type of the exception
    inline virtual FidanErrorType getType() const = 0;

    inline void addTrace(const std::string &file, const std::string &function, int line)
    {
        std::stringstream trace;
        trace << "  --> File \"" << file << "\", line " << line << ", in " << function;
        traceback.push_back(trace.str());
    }

    std::string getTraceback() const
    {
        std::stringstream traceString;
        traceString << "Traceback (most recent call last):" << std::endl;
        for (const auto &trace : traceback)
        {
            traceString << trace << std::endl;
        }
        return traceString.str();
    }
};

// TraceGuard class to automatically manage traceback
class TraceGuard
{
    FidanException &exception; // Reference to the exception
    std::string file;
    std::string function;
    int line;

public:
    TraceGuard(FidanException &ex, const std::string &file, const std::string &function, int line)
        : exception(ex), file(file), function(function), line(line)
    {
        // Automatically add traceback entry on creation
        exception.addTrace(file, function, line);
    }

    ~TraceGuard()
    {
        // Destructor automatically removes the trace entry
    }
};

// SyntaxError class
class SyntaxError : public FidanException
{
public:
    SyntaxError(const std::string &msg, int line = -1, int column = -1)
        : FidanException("SyntaxError: " + msg, line, column) {}

    inline FidanErrorType getType() const override
    {
        return FidanErrorType::SyntaxError;
    }
};

class ValueError : public FidanException
{
public:
    ValueError(const std::string &msg, int line = -1, int column = -1)
        : FidanException("ValueError: " + msg, line, column) {}

    inline FidanErrorType getType() const override
    {
        return FidanErrorType::RuntimeError;
    }
};

class TypeError : public FidanException
{
public:
    TypeError(const std::string &msg, int line = -1, int column = -1)
        : FidanException("TypeError: " + msg, line, column) {}

    inline FidanErrorType getType() const override
    {
        return FidanErrorType::TypeError;
    }
};

// DeclarationError class
class DeclarationError : public FidanException
{
public:
    DeclarationError(const std::string &msg, int line = -1, int column = -1)
        : FidanException("DeclarationError: " + msg, line, column) {}

    inline FidanErrorType getType() const override
    {
        return FidanErrorType::DeclarationError;
    }
};

// RuntimeError class
class RuntimeError : public FidanException
{
public:
    RuntimeError(const std::string &msg, int line = -1, int column = -1)
        : FidanException("RuntimeError: " + msg, line, column) {}

    inline FidanErrorType getType() const override
    {
        return FidanErrorType::RuntimeError;
    }
};

#endif // FIDAN_ERRORS_H