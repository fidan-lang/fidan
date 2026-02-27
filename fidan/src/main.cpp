// Copyright (c) Kaan Gönüldinc (AppSolves). All rights reserved.
// See LICENSE file in the project root for full license information.

// Include necessary headers
#include "headers/tokenizer.h"
#include "headers/parser.h"
#include "headers/semantic_analyzer.h"
#include "headers/interpreter.h"
#include "headers/errors.h"
#include "headers/ast.h"
#include "headers/main.h"
#include "headers/helpers.h"

#include <iostream>
#include <fstream>
#include <filesystem>
#include <functional>
// https://github.com/p-ranav/argparse
#include <argparse/argparse.hpp>

// Function to run the REPL
int runREPL()
{
    // Print the welcome message
    print("Fidan v" + version + " REPL");
    print("COMMANDS:");
    print("--> Type 'help', 'license', or 'credits' for more information");
    print("--> Type 'exit' to exit or 'clear' to clear the screen", 2);

    std::string line;

    while (true)
    {
        // Get the input from the user
        print(">>> ", 0);
        std::getline(std::cin, line);

        // Check if the line is a command
        if (line == "exit")
        {
            break;
        }

        auto command = commands.find(line);
        if (command != commands.end())
        {
            command->second();
        }
        else
        {
            // Process the line as source code
            // (Assuming you have a function to handle this)
            // processSourceLine(line);
        }
    }

    return 0;
}

// Main function
int main(int argc, char *argv[])
{
    // Check if the debug mode is active
    if (isDebugMode)
    {
        print(">>>DEBUG MODE IS ACTIVE<<<", 2);
    }

    // Create the argument parser
    argparse::ArgumentParser program("fidan", version);

    program.add_argument("source_file")
        .help("defines the source file to run")
        .metavar("SOURCE_FILE")
        .default_value(std::string("REPL"))
        .nargs(1);

    try
    {
        program.parse_args(argc, argv);
    }
    catch (const std::exception &err)
    {
        std::cerr << err.what() << std::endl;
        std::cerr << program;
        return 1;
    }

    // Check if the source file is not provided or is REPL
    if (program.is_used("source_file") == false || upper(program.get<std::string>("source_file")) == "REPL")
    {
        return runREPL();
    }

    // Get the source file from the arguments and check if it exists/is valid
    const std::string filename = program.get<std::string>("source_file");
    if ((filename.rfind(".fdn") != filename.size() - 4) && (filename.rfind(".fidan") != filename.size() - 6))
    {
        std::cerr << "FidanFileError: File must have '.fdn' or '.fidan' extension" << std::endl;
        return 1;
    }

    std::ifstream file(filename, std::ios::binary);
    if (!file || !file.good())
    {
        std::cerr << "FidanFileError: File '" << filename << "' not found" << std::endl;
        return 1;
    }
    if (!file.is_open())
    {
        std::cerr << "FidanFileError: Failed to open file '" << filename << "'" << std::endl;
        return 1;
    }

    // Read the file contents into a string
    const std::string source((std::istreambuf_iterator<char>(file)),
                             std::istreambuf_iterator<char>());

    // Read, tokenize, parse, analyze and interpret the source code
    try
    {
        const std::string full_file_path = std::filesystem::absolute(filename).string();
        Tokenizer tokenizer(source, full_file_path);
        const std::vector<Token> tokens = tokenizer.tokenize();

        Parser parser(tokens, full_file_path);
        const std::vector<std::unique_ptr<ASTNode>> ast = parser.parse();

        // Check if the debug mode is active
        if (isDebugMode)
        {
            print("ABSTRACT SYNTAX TREE: ", 2);
            // Print the AST
            for (const auto &statement : ast)
            {
                print(statement->toString());
            }
            print("");
        }

        SemanticAnalyzer analyzer(tokens, ast, full_file_path);
        analyzer.analyze();

        // InterpreterVM interpreter(ast);
        // interpreter.interpret();

        return 0;
    }
    // Catch and handle exceptions
    catch (const FidanException &e)
    {
        const std::unordered_map<int, std::string> sourceMap = preprocessSource(source);
        std::cerr << e.getTraceback() << std::endl;
        std::cerr << "  " << getSourceLine(e.getLine(), sourceMap) << std::endl;
        std::cerr << "  " << std::string(e.getColumn() - 2, ' ') << "^" << std::endl;
        std::cerr << std::endl;
        std::cerr << e.what() << " at line " << e.getLine() << ", column " << e.getColumn() << std::endl;
        return 1;
    }
    catch (const std::exception &e)
    {
        std::cerr << "RuntimeError: " << e.what() << std::endl;
        return 1;
    }
    catch (...)
    {
        std::cerr << "UnknownError: An unknown error occurred" << std::endl;
        return 1;
    }
}