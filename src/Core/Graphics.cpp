#include "Graphics.h"

#include <GL/glew.h>
#include <GLFW/glfw3.h>

#include "GLCheck.h"

Graphics::Graphics(Window &window)
        : m_wnd(window)
{
    glewInit();
}

void Graphics::DrawArrays()
{
    glCheck(glDrawArrays(GL_TRIANGLES, 0, 3));
}

void Graphics::BeginFrame(float r, float g, float b, float a)
{
    glCheck(glClearColor(r, g, b, a));
    glCheck(glClear(GL_COLOR_BUFFER_BIT));
}

void Graphics::EndFrame()
{
    glfwSwapBuffers(m_wnd.GetCoreWindow());
    glfwPollEvents();
}
