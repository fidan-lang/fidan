cmake_minimum_required(VERSION 3.10)

# Set the project name
project(Fidan)

# Include the FetchContent module
include(FetchContent)

# Fetch the external argparse repository
FetchContent_Declare(
  argparse
  GIT_REPOSITORY https://github.com/p-ranav/argparse.git
  GIT_TAG        master  # You can specify a specific tag or branch if needed
)

# Make the content available
FetchContent_MakeAvailable(argparse)

# Set default compiler flags based on the build type
if(NOT CMAKE_BUILD_TYPE OR CMAKE_BUILD_TYPE STREQUAL "")
  set(CMAKE_BUILD_TYPE "Release" CACHE STRING "Choose the type of build." FORCE)
endif()

# Specify the C++ standard
set(CMAKE_CXX_STANDARD 20)
set(CMAKE_CXX_STANDARD_REQUIRED True)

# Specify the source files and include directories
file(GLOB_RECURSE SOURCES "fidan/src/*.cpp")
include_directories("fidan/src/headers")

# Include the argparse directory
include_directories(${argparse_SOURCE_DIR}/include)

# Create the executable target
add_executable(fidan ${SOURCES})

# Set compiler flags based on the compiler
if(MSVC)
    # MSVC flags for Release configuration
    set(CMAKE_CXX_FLAGS_RELEASE "/O2 /GL /fp:fast /arch:AVX2")  # Optimize for speed
    # Add the /LTCG flag for link-time code generation
    set(CMAKE_EXE_LINKER_FLAGS_RELEASE "${CMAKE_EXE_LINKER_FLAGS_RELEASE} /LTCG")
    # MSVC flags for Debug configuration
    set(CMAKE_CXX_FLAGS_DEBUG "/Zi /Od")  # Debugging symbols, no optimization
else()
    # GCC/Clang flags for Release configuration
    set(CMAKE_CXX_FLAGS_RELEASE "-O3 -march=native -flto=4 -funroll-loops -finline-functions -fomit-frame-pointer -fuse-linker-plugin")
    # GCC/Clang flags for Debug configuration
    set(CMAKE_CXX_FLAGS_DEBUG "-g")  # Debugging symbols
endif()

# Add a definition for Debug mode using generator expressions
# Check if we are on Windows
if(WIN32)
    # For Windows, use target_compile_definitions
    target_compile_definitions(fidan PRIVATE
        $<$<CONFIG:Debug>:DEBUG_MODE>
    )
else()
    # For Linux/Other platforms, use add_definitions
    if(CMAKE_BUILD_TYPE STREQUAL "Debug")
        add_definitions(-DDEBUG_MODE)
    endif()
endif()

# Apply the appropriate compiler flags
set(CMAKE_CXX_FLAGS "${CMAKE_CXX_FLAGS} ${CMAKE_CXX_FLAGS_${CMAKE_BUILD_TYPE}}")

# Set the output directory for the executable, without subdirectories
set_target_properties(fidan PROPERTIES
    RUNTIME_OUTPUT_DIRECTORY "${CMAKE_CURRENT_SOURCE_DIR}/bin"
    RUNTIME_OUTPUT_DIRECTORY_RELEASE "${CMAKE_CURRENT_SOURCE_DIR}/bin"
    RUNTIME_OUTPUT_DIRECTORY_DEBUG "${CMAKE_CURRENT_SOURCE_DIR}/bin"
)

# Install the executable
install(TARGETS fidan
    RUNTIME DESTINATION bin
)

# Install the headers
install(DIRECTORY fidan/builtins/
    DESTINATION include/builtins
    FILES_MATCHING PATTERN "*.h"
)