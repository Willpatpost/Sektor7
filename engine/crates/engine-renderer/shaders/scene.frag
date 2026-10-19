#version 450

layout(location = 0) in vec3 vcol;

layout(location = 0) out vec4 frag;

void main() {
    frag = vec4(vcol, 1.0);
}
