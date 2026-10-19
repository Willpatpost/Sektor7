#version 450

layout(push_constant) uniform Push {
    mat4 mvp;
} push;

layout(location = 0) in vec3 pos;
layout(location = 1) in vec3 col;

layout(location = 0) out vec3 vcol;

void main() {
    vcol = col;
    gl_Position = push.mvp * vec4(pos, 1.0);
}
