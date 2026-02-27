// Copyright (c) Kaan Gönüldinc (AppSolves). All rights reserved.
// See LICENSE file in the project root for full license information.

#ifndef FIDAN_AST_H
#define FIDAN_AST_H

// Include necessary headers
#include <string>
#include <string_view>
#include <vector>
#include <memory>
#include <numeric>
#include <unordered_map>
#include <sstream>
#include "errors.h"
#include "tokenizer.h"

// TODO: Implement comparison between two AST nodes in O(1) time and in dependency of a specific attribute

// Enum class for the type of the AST node
enum class NodeType
{
    VariableDeclaration,
    VariableAssignment,
    ActionDeclaration,
    ObjectDeclaration,
    Block,
    Literal,
    ListLiteral,
    DynamicLiteral,
    BinaryExpression,
    UnaryExpression,
    VariableReference,
    FunctionCall,
    ReturnStatement,
    Decorator,
    NonNullExpression,
    WhenStatement,
    TryCatchStatement,
    ThrowStatement,
    ForLoop,
    WhileLoop
};

// Forward declaration of the ASTNode class
class ASTNode
{
public:
    // The index of the token that starts the AST node
    size_t firstTokenIndex;

    // Constructor
    explicit ASTNode(size_t firstTokenIndex) : firstTokenIndex(firstTokenIndex) {}
    // Destructor
    virtual ~ASTNode() = default;
    // Method to convert the AST node to a string
    virtual std::string toString() const = 0;
    // Method to get the type of the AST nodes
    inline virtual NodeType getNodeType() const = 0;
};

// Struct for the parameter of an action or loop
struct Parameter
{
    // Name of the parameter
    std::string_view name;
    // Whether the parameter is optional
    bool isOptional;
    // Type of the parameter
    std::string_view type;
    // Default value of the parameter
    std::unique_ptr<ASTNode> defaultValue;
};

// Enum class for the type of the scope
enum class ScopeType
{
    Global,
    Local,
    Nonlocal,
    ObjectPaired,
    DecoratorPaired
};

// Struct for the scope
struct Scope
{
    // Name of the scope
    std::string_view name;
    // Type of the scope
    ScopeType type;
    // Statements within the scope
    std::vector<std::unique_ptr<ASTNode>> statements;
    // Children of the scope
    std::vector<std::shared_ptr<Scope>> children;
    // Parent of the scope
    std::weak_ptr<Scope> parent;

    // Constructor
    explicit Scope(std::string_view name, ScopeType type, std::shared_ptr<Scope> parent = nullptr)
        : name(name), type(type), parent(parent) {}
};

// Class for the scope manager that manages the scopes
class ScopeManager
{
private:
    // Vector of scopes managed by the scope manager
    std::vector<std::shared_ptr<Scope>> scopes;

public:
    // Enter a new scope
    inline void enterScope(std::string_view name, ScopeType type)
    {
        std::shared_ptr<Scope> parent = scopes.empty() ? nullptr : scopes.back();
        scopes.push_back(std::make_shared<Scope>(name, type, parent));
    }

    // Exit the current scope
    inline void exitScope()
    {
        if (!scopes.empty())
        {
            scopes.pop_back();
        }
    }

    // Get the current scope
    inline std::shared_ptr<Scope> currentScope()
    {
        return scopes.empty() ? nullptr : scopes.back();
    }
};

// Class for the AST node representing a variable declaration
class VariableDeclaration : public ASTNode
{
public:
    // Parts of the identifier, e.g., `["this", "variable"]` for `this.variable`
    std::vector<std::string_view> identifierParts;
    // Type of the variable
    std::string_view type;
    // Initializer of the variable
    std::unique_ptr<ASTNode> initializer;
    // Scope of the variable
    std::shared_ptr<Scope> scope;

    // Constructor
    explicit VariableDeclaration(size_t firstTokenIndex, const std::vector<std::string_view> &identifierParts, std::string_view type, std::unique_ptr<ASTNode> initializer, std::shared_ptr<Scope> scope)
        : ASTNode(firstTokenIndex), identifierParts(identifierParts), type(type), initializer(std::move(initializer)), scope(scope) {}

    // Method to convert the AST node to a string
    std::string toString() const override
    {
        std::ostringstream oss;
        oss << "VariableDeclaration(name = " << std::accumulate(identifierParts.begin(), identifierParts.end(), std::string{}, [](const std::string &a, std::string_view b)
                                                                { return a + std::string(b); })
            << ", type = " << type
            << ", scope = " << (scope ? scope->name : "null")
            << ", scopeType = " << static_cast<int>(scope->type)
            << ", initializer = " << (initializer ? initializer->toString() : "null") << ")";
        return oss.str();
    }

    // Method to get the type of the AST nodes
    inline NodeType getNodeType() const override
    {
        return NodeType::VariableDeclaration;
    }
};

// Class for the AST node representing a variable assignment
class VariableAssignment : public ASTNode
{
public:
    // Parts of the identifier, e.g., `["this", "variable"]` for `this.variable`
    std::vector<std::string_view> identifierParts;
    // Value to assign to the variable
    std::unique_ptr<ASTNode> value;
    // Scope of the variable
    std::shared_ptr<Scope> scope;

    // Constructor
    explicit VariableAssignment(size_t firstTokenIndex, const std::vector<std::string_view> &identifierParts, std::unique_ptr<ASTNode> value, std::shared_ptr<Scope> scope)
        : ASTNode(firstTokenIndex), identifierParts(identifierParts), value(std::move(value)), scope(scope) {}

    // Method to convert the AST node to a string
    std::string toString() const override
    {
        std::ostringstream oss;
        oss << "VariableAssignment(name = " << std::accumulate(identifierParts.begin(), identifierParts.end(), std::string{}, [](const std::string &a, std::string_view b)
                                                               { return a + std::string(b); })
            << ", scope = " << (scope ? scope->name : "null")
            << ", scopeType = " << static_cast<int>(scope->type)
            << ", value = " << (value ? value->toString() : "null") << ")";
        return oss.str();
    }

    // Method to get the type of the AST nodes
    inline NodeType getNodeType() const override
    {
        return NodeType::VariableAssignment;
    }
};

// Class for the AST node representing a block
class Block : public ASTNode
{
public:
    // Statements within the block
    std::vector<std::unique_ptr<ASTNode>> statements;
    // Scope of the block
    std::shared_ptr<Scope> scope;

    // Constructor
    explicit Block(size_t firstTokenIndex, std::shared_ptr<Scope> scope = nullptr) : ASTNode(firstTokenIndex), scope(scope) {}

    // Method to convert the AST node to a string
    std::string toString() const override
    {
        std::ostringstream oss;
        oss << "Block(" << statements.size() << " statements, scope = " << (scope ? scope->name : "null") << ")";
        return oss.str();
    }

    // Method to get the type of the AST nodes
    inline NodeType getNodeType() const override
    {
        return NodeType::Block;
    }
};

// Class for the AST node representing an action/function declaration
class ActionDeclaration : public ASTNode
{
public:
    // Name of the action/function
    std::string_view name;
    // Name of the parent object
    std::string_view parentName;
    // Parameters of the action/function
    std::vector<Parameter> parameters;
    // Return type of the action/function
    std::string_view returnType;
    // Body of the action/function
    std::unique_ptr<ASTNode> body;
    // Scope of the action/function
    std::shared_ptr<Scope> scope;

    // Constructor
    explicit ActionDeclaration(size_t firstTokenIndex, std::string_view name, std::string_view parentName, std::vector<Parameter> parameters, std::string_view returnType, std::unique_ptr<ASTNode> body, std::shared_ptr<Scope> scope)
        : ASTNode(firstTokenIndex), name(name), parentName(parentName), parameters(std::move(parameters)), returnType(returnType), body(std::move(body)), scope(scope)
    {
        // Check if the body is of type <Block>
        if (body && body->getNodeType() != NodeType::Block)
        {
            throw RuntimeError("'Action/Function' body must be of type <Block>", -1, -1);
        }
    }

    // Method to convert the AST node to a string
    std::string toString() const override
    {
        std::ostringstream oss;
        oss << "ActionDeclaration(" << name << ", parent = " << (parentName.empty() ? "None" : parentName) << ", parameters = [";

        for (size_t i = 0; i < parameters.size(); ++i)
        {
            const auto &param = parameters[i];
            oss << (param.isOptional ? "optional " : "") << param.name << " oftype " << (param.type.empty() ? "any" : param.type);

            if (param.defaultValue)
            {
                oss << " default " << param.defaultValue->toString();
            }

            if (i < parameters.size() - 1)
            {
                oss << ", ";
            }
        }

        oss << "], body = " << (body ? body->toString() : "null")
            << ", returnType = " << returnType
            << ", scope = " << (scope ? scope->name : "null") << ")";
        return oss.str();
    }

    // Method to get the type of the AST nodes
    inline NodeType getNodeType() const override
    {
        return NodeType::ActionDeclaration;
    }
};

// Class for the AST node representing an object declaration
class ObjectDeclaration : public ASTNode
{
public:
    // Name of the object
    std::string_view name;
    // Name of the extended parent object
    std::string_view parentName;
    // Statements within the object
    std::vector<std::unique_ptr<ASTNode>> statements;
    // Scope of the object
    std::shared_ptr<Scope> scope;

    // Constructor
    explicit ObjectDeclaration(size_t firstTokenIndex, std::string_view name, std::string_view parentName, std::shared_ptr<Scope> scope = nullptr)
        : ASTNode(firstTokenIndex), name(name), parentName(parentName), scope(scope) {}

    // Method to convert the AST node to a string
    std::string toString() const override
    {
        std::ostringstream oss;
        oss << "ObjectDeclaration(" << name << ", parent = " << (parentName.empty() ? "None" : parentName)
            << ", scope = " << (scope ? scope->name : "null")
            << ", " << statements.size() << " statements)";
        return oss.str();
    }

    // Method to get the type of the AST nodes
    inline NodeType getNodeType() const override
    {
        return NodeType::ObjectDeclaration;
    }
};

// Class for the AST node representing a literal
class Literal : public ASTNode
{
public:
    // Value of the literal
    Token value;
    // Type of the literal
    std::string_view type;
    // Scope of the literal
    std::shared_ptr<Scope> scope;

    // Constructor
    explicit Literal(size_t firstTokenIndex, Token value, std::string_view type, std::shared_ptr<Scope> scope = nullptr) : ASTNode(firstTokenIndex), value(value), type(type), scope(scope) {}

    // Method to convert the AST node to a string
    std::string toString() const override
    {
        std::ostringstream oss;
        oss << "Literal(" << value.value << ", type = " << type << ", scope = " << (scope ? scope->name : "null") << ")";
        return oss.str();
    }

    // Method to get the type of the AST nodes
    inline NodeType getNodeType() const override
    {
        return NodeType::Literal;
    }
};

// Class for the AST node representing a list literal
class ListLiteral : public ASTNode
{
public:
    // Elements of the list literal
    std::vector<std::unique_ptr<ASTNode>> elements;
    // Scope of the list literal
    std::shared_ptr<Scope> scope;

    // Constructor
    explicit ListLiteral(size_t firstTokenIndex, std::vector<std::unique_ptr<ASTNode>> elements, std::shared_ptr<Scope> scope = nullptr)
        : ASTNode(firstTokenIndex), elements(std::move(elements)), scope(scope) {}

    // Method to convert the AST node to a string
    std::string toString() const override
    {
        std::ostringstream oss;
        oss << "ListLiteral(" << elements.size() << " elements, scope = " << (scope ? scope->name : "null") << ")";
        return oss.str();
    }

    // Method to get the type of the AST nodes
    inline NodeType getNodeType() const override
    {
        return NodeType::ListLiteral;
    }
};

// Class for the AST node representing a dynamic literal
class DynamicLiteral : public ASTNode
{
public:
    // Scope of the dynamic literal
    std::shared_ptr<Scope> scope;

    // Constructor
    explicit DynamicLiteral(size_t firstTokenIndex, std::shared_ptr<Scope> scope = nullptr) : ASTNode(firstTokenIndex), scope(scope) {}

    // Method to convert the AST node to a string
    std::string toString() const override
    {
        std::ostringstream oss;
        oss << "DynamicLiteral(scope = " << (scope ? scope->name : "null") << ")";
        return oss.str();
    }

    // Method to get the type of the AST nodes
    inline NodeType getNodeType() const override
    {
        return NodeType::DynamicLiteral;
    }
};

// Class for the AST node representing a binary expression
class BinaryExpression : public ASTNode
{
public:
    // Left operand of the binary expression
    std::unique_ptr<ASTNode> left;
    // Right operand of the binary expression
    std::unique_ptr<ASTNode> right;
    // Operator of the binary expression
    Token op;
    // Scope of the binary expression
    std::shared_ptr<Scope> scope;

    // Constructor
    explicit BinaryExpression(size_t firstTokenIndex, std::unique_ptr<ASTNode> left, Token op, std::unique_ptr<ASTNode> right, std::shared_ptr<Scope> scope = nullptr)
        : ASTNode(firstTokenIndex), left(std::move(left)), right(std::move(right)), op(op), scope(scope) {}

    // Method to convert the AST node to a string
    std::string toString() const override
    {
        std::ostringstream oss;
        oss << "BinaryExpression(" << left->toString() << " " << op.value << " " << right->toString()
            << ", scope = " << (scope ? scope->name : "null") << ")";
        return oss.str();
    }

    // Method to get the type of the AST nodes
    inline NodeType getNodeType() const override
    {
        return NodeType::BinaryExpression;
    }
};

// Class for the AST node representing a unary expression
class UnaryExpression : public ASTNode
{
public:
    // Operand of the unary expression
    std::unique_ptr<ASTNode> operand;
    // Operator of the unary expression
    Token op;
    // Scope of the unary expression
    std::shared_ptr<Scope> scope;

    // Constructor
    explicit UnaryExpression(size_t firstTokenIndex, Token op, std::unique_ptr<ASTNode> operand, std::shared_ptr<Scope> scope = nullptr)
        : ASTNode(firstTokenIndex), operand(std::move(operand)), op(op), scope(scope) {}

    // Method to convert the AST node to a string
    std::string toString() const override
    {
        std::ostringstream oss;
        oss << "UnaryExpression(" << op.value << operand->toString()
            << ", scope = " << (scope ? scope->name : "null") << ")";
        return oss.str();
    }

    // Method to get the type of the AST nodes
    inline NodeType getNodeType() const override
    {
        return NodeType::UnaryExpression;
    }
};

// Class for the AST node representing a variable reference
class VariableReference : public ASTNode
{
public:
    // Parts of the identifier, e.g., `["this", "variable"]` for `this.variable`
    std::vector<std::string_view> identifierParts;
    // Scope of the variable reference
    std::shared_ptr<Scope> scope;

    // Constructor
    explicit VariableReference(size_t firstTokenIndex, const std::vector<std::string_view> &identifierParts, std::shared_ptr<Scope> scope = nullptr)
        : ASTNode(firstTokenIndex), identifierParts(identifierParts), scope(scope) {}

    // Method to convert the AST node to a string
    std::string toString() const override
    {
        std::ostringstream oss;
        oss << "VariableReference(" << std::accumulate(identifierParts.begin(), identifierParts.end(), std::string{}, [](const std::string &a, std::string_view b)
                                                       { return a + std::string(b); })
            << ", scope = " << (scope ? scope->name : "null") << ")";
        return oss.str();
    }

    // Method to get the type of the AST nodes
    inline NodeType getNodeType() const override
    {
        return NodeType::VariableReference;
    }
};

// Class for the AST node representing a function call
class FunctionCall : public ASTNode
{
public:
    // Parts of the identifier, e.g., `["this", "variable"]` for `this.variable`
    std::vector<std::string_view> identifierParts;
    // Arguments of the function call
    std::vector<std::unique_ptr<ASTNode>> arguments;
    // Keyword arguments of the function call
    std::unordered_map<std::string_view, std::unique_ptr<ASTNode>> keywordArguments;
    // Scope of the function call
    std::shared_ptr<Scope> scope;

    // Constructor
    explicit FunctionCall(size_t firstTokenIndex, std::vector<std::string_view> identifierParts, std::vector<std::unique_ptr<ASTNode>> arguments, std::unordered_map<std::string_view, std::unique_ptr<ASTNode>> keywordArguments, std::shared_ptr<Scope> scope = nullptr)
        : ASTNode(firstTokenIndex), identifierParts(std::move(identifierParts)), arguments(std::move(arguments)), keywordArguments(std::move(keywordArguments)), scope(scope) {}

    // Method to convert the AST node to a string
    std::string toString() const override
    {
        std::ostringstream oss;
        oss << "FunctionCall(" << std::accumulate(identifierParts.begin(), identifierParts.end(), std::string{}, [](const std::string &a, std::string_view b)
                                                  { return a + std::string(b); })
            << ", arguments = [";

        for (size_t i = 0; i < arguments.size(); ++i)
        {
            oss << arguments[i]->toString();

            if (i < arguments.size() - 1)
            {
                oss << ", ";
            }
        }

        oss << "], keyword_arguments = [";

        for (auto it = keywordArguments.begin(); it != keywordArguments.end(); ++it)
        {
            oss << it->first << " = " << it->second->toString();

            if (std::next(it) != keywordArguments.end())
            {
                oss << ", ";
            }
        }

        oss << "], scope = " << (scope ? scope->name : "null") << ")";
        return oss.str();
    }

    // Method to get the type of the AST nodes
    inline NodeType getNodeType() const override
    {
        return NodeType::FunctionCall;
    }
};

// Class for the AST node representing a return statement
class ReturnStatement : public ASTNode
{
public:
    // Value to return
    std::unique_ptr<ASTNode> value;
    // Scope of the return statement
    std::shared_ptr<Scope> scope;

    // Constructor
    explicit ReturnStatement(size_t firstTokenIndex, std::unique_ptr<ASTNode> value, std::shared_ptr<Scope> scope = nullptr)
        : ASTNode(firstTokenIndex), value(std::move(value)), scope(scope) {}

    // Method to convert the AST node to a string
    std::string toString() const override
    {
        std::ostringstream oss;
        oss << "ReturnStatement(" << (value ? value->toString() : "null")
            << ", scope = " << (scope ? scope->name : "null") << ")";
        return oss.str();
    }

    // Method to get the type of the AST nodes
    inline NodeType getNodeType() const override
    {
        return NodeType::ReturnStatement;
    }
};

// Class for the AST node representing a decorator
class Decorator : public ASTNode
{
public:
    // Name of the decorator
    std::string_view name;
    // Statement to decorate
    std::unique_ptr<ASTNode> statement;
    // Scope of the decorator
    std::shared_ptr<Scope> scope;

    // Constructor
    explicit Decorator(size_t firstTokenIndex, std::string_view name, std::unique_ptr<ASTNode> statement, std::shared_ptr<Scope> scope = nullptr)
        : ASTNode(firstTokenIndex), name(name), statement(std::move(statement)), scope(scope) {}

    // Method to convert the AST node to a string
    std::string toString() const override
    {
        std::ostringstream oss;
        oss << "Decorator(" << name << ", statement = " << (statement ? statement->toString() : "null")
            << ", scope = " << (scope ? scope->name : "null") << ")";
        return oss.str();
    }

    // Method to get the type of the AST nodes
    inline NodeType getNodeType() const override
    {
        return NodeType::Decorator;
    }
};

// Class for the AST node representing a non-null expression
class NonNullExpression : public ASTNode
{
public:
    // Left operand of the non-null expression
    std::unique_ptr<ASTNode> left;
    // Right operand of the non-null expression
    std::unique_ptr<ASTNode> right;
    // Scope of the non-null expression
    std::shared_ptr<Scope> scope;

    // Constructor
    explicit NonNullExpression(size_t firstTokenIndex, std::unique_ptr<ASTNode> left, std::unique_ptr<ASTNode> right, std::shared_ptr<Scope> scope = nullptr)
        : ASTNode(firstTokenIndex), left(std::move(left)), right(std::move(right)), scope(scope) {}

    // Method to convert the AST node to a string
    std::string toString() const override
    {
        std::ostringstream oss;
        oss << "NonNullExpression(" << left->toString() << " ?? " << right->toString()
            << ", scope = " << (scope ? scope->name : "null") << ")";
        return oss.str();
    }

    // Method to get the type of the AST nodes
    inline NodeType getNodeType() const override
    {
        return NodeType::NonNullExpression;
    }
};

// Class for the AST node representing a when statement
class WhenStatement : public ASTNode
{
public:
    // Conditions and blocks of the when statement
    std::vector<std::pair<std::unique_ptr<ASTNode>, std::unique_ptr<ASTNode>>> conditionsAndBlocks;
    // Scope of the when statement
    std::shared_ptr<Scope> scope;

    // Constructor
    explicit WhenStatement(size_t firstTokenIndex, std::vector<std::pair<std::unique_ptr<ASTNode>, std::unique_ptr<ASTNode>>> &&conditionsAndBlocks, std::shared_ptr<Scope> scope = nullptr)
        : ASTNode(firstTokenIndex), conditionsAndBlocks(std::move(conditionsAndBlocks)), scope(scope)
    {
        // Reserve the necessary space for the conditions and blocks
        this->conditionsAndBlocks.reserve(this->conditionsAndBlocks.size());

        for (const auto &conditionAndBlock : this->conditionsAndBlocks)
        {
            if (conditionAndBlock.second && conditionAndBlock.second->getNodeType() != NodeType::Block)
            {
                throw RuntimeError("'If/When' body must be of type <Block>", -1, -1);
            }
        }
    }

    // Method to convert the AST node to a string
    std::string toString() const override
    {
        std::ostringstream oss;
        oss << "WhenStatement(" << conditionsAndBlocks.size() << " conditions and blocks"
            << ", scope = " << (scope ? scope->name : "null") << ")";
        return oss.str();
    }

    // Method to get the type of the AST nodes
    inline NodeType getNodeType() const override
    {
        return NodeType::WhenStatement;
    }
};

// Class for the AST node representing a try-catch statement
class TryCatchStatement : public ASTNode
{
public:
    // Body of the try-catch statement
    std::unique_ptr<ASTNode> body;
    // Identifier of the catch block
    std::string_view catchIdentifier;
    // Body of the catch block
    std::unique_ptr<ASTNode> catchBody;
    // Body of the finally block
    std::unique_ptr<ASTNode> finallyBlock;
    // Body of the else block
    std::unique_ptr<ASTNode> elseBlock;
    // Scope of the try-catch statement
    std::shared_ptr<Scope> scope;

    // Constructor
    explicit TryCatchStatement(size_t firstTokenIndex, std::unique_ptr<ASTNode> body, std::string_view catchIdentifier, std::unique_ptr<ASTNode> catchBody, std::unique_ptr<ASTNode> finallyBlock, std::unique_ptr<ASTNode> elseBlock, std::shared_ptr<Scope> scope = nullptr)
        : ASTNode(firstTokenIndex), body(std::move(body)), catchIdentifier(catchIdentifier), catchBody(std::move(catchBody)), finallyBlock(std::move(finallyBlock)), elseBlock(std::move(elseBlock)), scope(scope)
    {
        // Check if the bodies are of type <Block>
        if (body && body->getNodeType() != NodeType::Block)
        {
            throw RuntimeError("'Try/Attempt' body must be of type <Block>", -1, -1);
        }
        if (catchBody && catchBody->getNodeType() != NodeType::Block)
        {
            throw RuntimeError("'Catch/Except' body must be of type <Block>", -1, -1);
        }
        if (finallyBlock && finallyBlock->getNodeType() != NodeType::Block)
        {
            throw RuntimeError("'Finally/Anyway' body must be of type <Block>", -1, -1);
        }
        if (elseBlock && elseBlock->getNodeType() != NodeType::Block)
        {
            throw RuntimeError("'Else/Otherwise' body must be of type <Block>", -1, -1);
        }
    }

    // Method to convert the AST node to a string
    std::string toString() const override
    {
        std::ostringstream oss;
        oss << "TryCatchStatement(body = " << body->toString()
            << ", catchIdentifier = " << catchIdentifier
            << ", catchBody = " << (catchBody ? catchBody->toString() : "null")
            << ", finallyBlock = " << (finallyBlock ? finallyBlock->toString() : "null")
            << ", elseBlock = " << (elseBlock ? elseBlock->toString() : "null")
            << ", scope = " << (scope ? scope->name : "null") << ")";
        return oss.str();
    }

    // Method to get the type of the AST nodes
    inline NodeType getNodeType() const override
    {
        return NodeType::TryCatchStatement;
    }
};

// Class for the AST node representing a throw statement
class ThrowStatement : public ASTNode
{
public:
    // Value to throw
    std::unique_ptr<ASTNode> value;
    // Scope of the throw statement
    std::shared_ptr<Scope> scope;

    // Constructor
    explicit ThrowStatement(size_t firstTokenIndex, std::unique_ptr<ASTNode> value, std::shared_ptr<Scope> scope = nullptr)
        : ASTNode(firstTokenIndex), value(std::move(value)), scope(scope) {}

    // Method to convert the AST node to a string
    std::string toString() const override
    {
        std::ostringstream oss;
        oss << "ThrowStatement(" << (value ? value->toString() : "null")
            << ", scope = " << (scope ? scope->name : "null") << ")";
        return oss.str();
    }

    // Method to get the type of the AST nodes
    inline NodeType getNodeType() const override
    {
        return NodeType::ThrowStatement;
    }
};

// Class for the AST node representing a for loop
class ForLoop : public ASTNode
{
public:
    // Parameters of the for loop
    std::vector<Parameter> parameters;
    // Iterable of the for loop
    std::unique_ptr<ASTNode> iterable;
    // Body of the for loop
    std::unique_ptr<ASTNode> body;

    // Constructor
    explicit ForLoop(size_t firstTokenIndex, std::vector<Parameter> parameters, std::unique_ptr<ASTNode> iterable, std::unique_ptr<ASTNode> body)
        : ASTNode(firstTokenIndex), parameters(std::move(parameters)), iterable(std::move(iterable)), body(std::move(body))
    {
        // Check if the body is of type <Block>
        if (body && body->getNodeType() != NodeType::Block)
        {
            throw RuntimeError("'For/ForEach' body must be of type <Block>", -1, -1);
        }
    }

    // Method to convert the AST node to a string
    std::string toString() const override
    {
        std::ostringstream oss;
        oss << "ForLoop(parameters = [";

        for (size_t i = 0; i < parameters.size(); ++i)
        {
            const auto &param = parameters[i];
            oss << (param.isOptional ? "optional " : "") << param.name << " oftype " << (param.type.empty() ? "any" : param.type);

            if (param.defaultValue)
            {
                oss << " default " << param.defaultValue->toString();
            }

            if (i < parameters.size() - 1)
            {
                oss << ", ";
            }
        }

        oss << "], iterable = " << iterable->toString()
            << ", body = " << (body ? body->toString() : "null") << ")";
        return oss.str();
    }

    // Method to get the type of the AST nodes
    inline NodeType getNodeType() const override
    {
        return NodeType::ForLoop;
    }
};

// Class for the AST node representing a while loop
class WhileLoop : public ASTNode
{
public:
    // Condition of the while loop
    std::unique_ptr<ASTNode> condition;
    // Body of the while loop
    std::unique_ptr<ASTNode> body;

    // Constructor
    explicit WhileLoop(size_t firstTokenIndex, std::unique_ptr<ASTNode> condition, std::unique_ptr<ASTNode> body)
        : ASTNode(firstTokenIndex), condition(std::move(condition)), body(std::move(body))
    {
        // Check if the body is of type <Block>
        if (body && body->getNodeType() != NodeType::Block)
        {
            throw RuntimeError("'While/AsLongAs' body must be of type <Block>", -1, -1);
        }
    }

    // Method to convert the AST node to a string
    std::string toString() const override
    {
        std::ostringstream oss;
        oss << "WhileLoop(condition = " << condition->toString()
            << ", body = " << (body ? body->toString() : "null") << ")";
        return oss.str();
    }

    // Method to get the type of the AST nodes
    inline NodeType getNodeType() const override
    {
        return NodeType::WhileLoop;
    }
};

#endif // FIDAN_AST_H