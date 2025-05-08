<template>
  <div class="dialog">
    <div class="dialog-title">
      <slot name="title"></slot>
    </div>
    <div class="dialog-desc">
      <slot name="desc"></slot>
    </div>
    <div class="dialog-body">
      <slot name="body"></slot>
    </div>
    <div class="dialog-footer">
      <slot name="footer"></slot>
    </div>
  </div>
</template>

<script setup lang="ts"></script>

<style scoped>
.dialog {
  position: absolute;
  top: 4px;
  left: 16px;
  right: 16px;
  bottom: 0;
}
</style>
